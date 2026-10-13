use super::{R2Scope, R2UserResources, error};
use crate::core::bucket::*;
use crate::core::project::ProjectId;
use crate::core::user::{UserId, Username};
use bytes::Bytes;
use futures::stream::BoxStream;
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use worker::{Bucket, D1Database};

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
struct Entry {
    key: String,
    #[serde(deserialize_with = "deserialize_extension")]
    extension: Option<String>,
}

fn deserialize_extension<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    let extension = Option::<String>::deserialize(deserializer)?;
    if extension.as_deref().is_some_and(|value| Extension::new(value).is_none()) {
        return Err(serde::de::Error::custom("invalid file extension"));
    }
    Ok(extension)
}

impl Entry {
    fn extension(&self) -> Option<Extension<'_>> {
        self.extension
            .as_deref()
            .map(|value| Extension::new(value).expect("Stored file extensions must be valid"))
    }
}

struct Snapshot {
    user_id: UserId,
    files: Vec<Entry>,
    revision: i64,
}

pub struct R2File<'a> {
    extension: Option<String>,
    source: Source<'a>,
}

enum Source<'a> {
    Stored {
        project_id: ProjectId,
        user_id: UserId,
        key: String,
        revision: i64,
    },
    New(BoxStream<'a, Result<Bytes, std::io::Error>>),
}

impl<'a> FileObject<'a> for R2File<'a> {
    fn from_data(data: BoxStream<'a, Result<Bytes, std::io::Error>>, extension: Option<Extension<'_>>) -> Self {
        Self {
            extension: extension.map(|extension| extension.as_ref().to_owned()),
            source: Source::New(data),
        }
    }
    fn extension(&self) -> Option<Extension<'_>> {
        self.extension
            .as_deref()
            .map(|value| Extension::new(value).expect("File extensions must be valid"))
    }
}

async fn physical_objects(bucket: &Bucket, prefix: &str) -> Result<Vec<worker::Object>, BucketError> {
    let mut objects = Vec::new();
    let mut cursor = None;
    loop {
        let mut list = bucket.list().prefix(prefix).limit(1000);
        if let Some(cursor) = cursor.take() {
            list = list.cursor(cursor);
        }
        let page = list.execute().await.map_err(error)?;
        objects.extend(page.objects());
        if !page.truncated() {
            break;
        }
        cursor = page.cursor();
    }
    Ok(objects)
}

async fn snapshot(resources: &R2UserResources<'_>) -> Result<Snapshot, BucketError> {
    #[derive(Deserialize)]
    struct Row {
        user_id: UserId,
        files: String,
        revision: i64,
    }
    let scope = &resources.scope;
    let row = worker::query!(
        scope.db,
        "SELECT m.user_id, COALESCE(r.files, '[]') AS files, COALESCE(r.revision, 0) AS revision FROM project_member m
         LEFT JOIN resource_file_list r ON r.project_id = m.project_id AND r.user_id = m.user_id
         WHERE m.project_id = ? AND m.username = ?",
        resources.project_id,
        resources.username
    )
    .map_err(error)?
    .first::<Row>(None)
    .await
    .map_err(error)?
    .ok_or(BucketError::InvalidFile)?;
    Ok(Snapshot {
        user_id: row.user_id,
        files: serde_json::from_str(&row.files).map_err(std::io::Error::other)?,
        revision: row.revision,
    })
}

async fn remove_objects(bucket: &Bucket, keys: Vec<String>) {
    if !keys.is_empty()
        && let Err(error) = bucket.delete_multiple(keys).await
    {
        tracing::warn!(%error, "Could not clean up unused resource files");
    }
}

impl UserResourceBucket for R2UserResources<'_> {
    type FileObject<'a> = R2File<'a>;

    #[worker::send]
    async fn current_file_list<'a>(&self) -> Result<Vec<Self::FileObject<'a>>, BucketError> {
        let snapshot = snapshot(self).await?;
        Ok(snapshot
            .files
            .into_iter()
            .map(|entry| R2File {
                extension: entry.extension,
                source: Source::Stored {
                    project_id: self.project_id,
                    user_id: snapshot.user_id,
                    key: entry.key,
                    revision: snapshot.revision,
                },
            })
            .collect())
    }

    #[worker::send]
    async fn update<'a>(&self, files: Vec<Self::FileObject<'a>>) -> Result<(), BucketError> {
        if files.len() > MAX_RESOURCE_FILES {
            return Err(BucketError::TooManyFiles);
        }
        let old = snapshot(self).await?;
        let mut retained = BTreeSet::new();
        for (index, file) in files.iter().enumerate() {
            self.scope.key(&resource_file_key(index, file.extension()))?;
            if let Source::Stored {
                project_id,
                user_id,
                key,
                revision,
            } = &file.source
            {
                if *project_id != self.project_id || *user_id != old.user_id || !retained.insert(key.clone()) {
                    return Err(BucketError::InvalidFile);
                }
                if *revision != old.revision || !old.files.iter().any(|entry| &entry.key == key && entry.extension == file.extension) {
                    return Err(BucketError::Conflict);
                }
            }
        }
        let storage = R2Scope {
            bucket: self.scope.bucket,
            db: self.scope.db,
            prefix: format!("projects/{}/resource-files/users/{}/", self.project_id, old.user_id),
        };
        let mut created = Vec::new();
        let result = async {
            let mut entries = Vec::with_capacity(files.len());
            for file in files {
                let key = match file.source {
                    Source::Stored { key, .. } => key,
                    Source::New(data) => {
                        let mut nonce = [0; 16];
                        getrandom::fill(&mut nonce).map_err(std::io::Error::other)?;
                        let name: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
                        let key = storage.key(&name)?;
                        storage.put(&name, data).await?;
                        created.push(key.clone());
                        key
                    }
                };
                entries.push(Entry {
                    key,
                    extension: file.extension,
                });
            }
            let json = serde_json::to_string(&entries).map_err(std::io::Error::other)?;
            let result = if old.revision == 0 {
                worker::query!(
                    self.scope.db,
                    "INSERT INTO resource_file_list (project_id, user_id, files, revision)
                     SELECT project_id, user_id, ?, 1 FROM project_member
                     WHERE project_id = ? AND username = ? AND user_id = ?
                     ON CONFLICT (project_id, user_id) DO NOTHING",
                    json,
                    self.project_id,
                    self.username,
                    old.user_id
                )
                .map_err(error)?
                .run()
                .await
                .map_err(error)?
            } else {
                worker::query!(
                    self.scope.db,
                    "UPDATE resource_file_list AS r SET files = ?, revision = r.revision + 1 FROM project_member m
                     WHERE r.project_id = m.project_id AND r.user_id = m.user_id
                       AND m.project_id = ? AND m.username = ? AND r.user_id = ? AND r.revision = ?",
                    json,
                    self.project_id,
                    self.username,
                    old.user_id,
                    old.revision
                )
                .map_err(error)?
                .run()
                .await
                .map_err(error)?
            };
            if result.meta().map_err(error)?.and_then(|meta| meta.changes) != Some(1) {
                return Err(BucketError::Conflict);
            }
            Ok(())
        }
        .await;
        if result.is_err() {
            remove_objects(self.scope.bucket, created).await;
        } else {
            remove_objects(
                self.scope.bucket,
                old.files
                    .into_iter()
                    .filter(|entry| !retained.contains(&entry.key))
                    .map(|entry| entry.key)
                    .collect(),
            )
            .await;
        }
        result
    }
}

/// Resolve a public, numbered resource path without exposing private object keys.
pub(crate) async fn resolve(db: &D1Database, project_id: ProjectId, key: &str) -> Result<Option<String>, BucketError> {
    let Some(path) = key.strip_prefix("authors/") else {
        return Ok(Some(format!("projects/{project_id}/resources/{key}")));
    };
    let Some((author, name)) = path.split_once('/') else {
        return Ok(None);
    };
    let row = worker::query!(
        db,
        "SELECT r.files FROM resource_file_list r
         JOIN project_member m ON m.project_id = r.project_id AND m.user_id = r.user_id
         WHERE r.project_id = ? AND m.username = ?",
        project_id,
        author
    )
    .map_err(error)?
    .first::<String>(Some("files"))
    .await
    .map_err(error)?;
    let Some(files) = row else {
        return Ok(None);
    };
    let files: Vec<Entry> = serde_json::from_str(&files).map_err(std::io::Error::other)?;
    for (index, file) in files.into_iter().enumerate() {
        if resource_file_key(index, file.extension()) == name {
            return Ok(Some(file.key));
        }
    }
    Ok(None)
}

#[derive(Debug, Deserialize)]
pub enum ResourceObjectPrefix<'a> {
    Root,
    RelativeFromRoot(&'a str),
    Authors,
    Author(Username),
    RelativeFromAuthor(Username, &'a str),
}

impl<'a> ResourceObjectPrefix<'a> {
    pub(crate) fn from_key(prefix: &'a str) -> Option<ResourceObjectPrefix<'a>> {
        if prefix.is_empty() {
            return Some(ResourceObjectPrefix::Root);
        }
        if prefix == "authors/" {
            return Some(ResourceObjectPrefix::Authors);
        }
        let Some((username, relative)) = prefix.strip_prefix("authors/").and_then(|path| path.split_once('/')) else {
            return Some(ResourceObjectPrefix::RelativeFromRoot(prefix));
        };
        let username = Username::new(username.into())?;
        Some(if relative.is_empty() {
            ResourceObjectPrefix::Author(username)
        } else {
            ResourceObjectPrefix::RelativeFromAuthor(username, relative)
        })
    }
}

/// Logical names and physical metadata for S3 listings (no file contents are read).
pub(crate) async fn objects(
    bucket: &Bucket,
    db: &D1Database,
    project_id: ProjectId,
    prefix: ResourceObjectPrefix<'_>,
) -> Result<BTreeMap<String, worker::Object>, BucketError> {
    #[derive(Deserialize)]
    struct ScopedRow {
        user_id: UserId,
        username: Username,
        files: String,
    }
    let (root_prefix, username, file_prefix) = match prefix {
        ResourceObjectPrefix::Root => ("", None, ""),
        ResourceObjectPrefix::RelativeFromRoot(prefix) => (prefix, None, ""),
        ResourceObjectPrefix::Authors => ("authors/", None, ""),
        ResourceObjectPrefix::Author(username) => ("authors/", Some(username), ""),
        ResourceObjectPrefix::RelativeFromAuthor(username, prefix) => ("authors/", Some(username), prefix),
    };
    let mut objects = BTreeMap::new();
    if !root_prefix.starts_with("authors/") {
        let base = format!("projects/{project_id}/resources/");
        for object in physical_objects(bucket, &format!("{base}{root_prefix}")).await? {
            let key = object.key();
            let relative = key.strip_prefix(&base).expect("R2 listing is scoped");
            if !relative.starts_with("authors/") {
                objects.insert(relative.to_owned(), object);
            }
        }
    }
    if !root_prefix.starts_with("authors/") && !"authors/".starts_with(root_prefix) {
        return Ok(objects);
    }
    let rows = if let Some(username) = &username {
        worker::query!(
            db,
            "SELECT r.user_id, m.username, r.files FROM resource_file_list r
             JOIN project_member m ON m.project_id = r.project_id AND m.user_id = r.user_id
             WHERE r.project_id = ? AND m.username = ?",
            project_id,
            username
        )
        .map_err(error)?
        .first::<ScopedRow>(None)
        .await
        .map_err(error)?
        .into_iter()
        .collect::<Vec<_>>()
    } else {
        // The prefix can end inside an author name or continue into a file name.
        worker::query!(
            db,
            "SELECT r.user_id, m.username, r.files FROM resource_file_list r
             JOIN project_member m ON m.project_id = r.project_id AND m.user_id = r.user_id
             WHERE r.project_id = ?
               AND (instr('authors/' || m.username || '/', ?) = 1 OR instr(?, 'authors/' || m.username || '/') = 1)",
            project_id,
            root_prefix,
            root_prefix
        )
        .map_err(error)?
        .all()
        .await
        .map_err(error)?
        .results::<ScopedRow>()
        .map_err(error)?
    };
    let mut files = BTreeMap::new();
    let mut users = BTreeSet::new();
    for row in rows {
        let entries: Vec<Entry> = serde_json::from_str(&row.files).map_err(std::io::Error::other)?;
        for (index, entry) in entries.into_iter().enumerate() {
            let name = resource_file_key(index, entry.extension());
            let key = format!("authors/{}/{name}", row.username);
            if key.starts_with(root_prefix) && name.starts_with(file_prefix) {
                users.insert(row.user_id);
                files.insert(entry.key, key);
            }
        }
    }
    if files.is_empty() {
        return Ok(objects);
    }
    let base = format!("projects/{project_id}/resource-files/users/");
    let prefixes = if username.is_none() && "authors/".starts_with(root_prefix) {
        vec![base]
    } else {
        users.into_iter().map(|user_id| format!("{base}{user_id}/")).collect()
    };
    for prefix in prefixes {
        for object in physical_objects(bucket, &prefix).await? {
            if let Some(key) = files.remove(&object.key()) {
                objects.insert(key, object);
            }
        }
    }
    Ok(objects)
}
