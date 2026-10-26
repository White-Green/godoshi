use crate::core::bucket::*;
use crate::core::metadata::*;
use crate::core::operation_log::{OperationLog, OperationLogEntry, OperationLogPage};
use crate::core::project::ProjectId;
use crate::core::user::{ProjectUserStore, UserId, UserStore, Username};
use bytes::Bytes;
use futures::stream::BoxStream;
use futures::{Stream, TryStreamExt};
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Bound::{Excluded, Unbounded};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use time::OffsetDateTime;

#[derive(Clone, Default)]
pub struct MemoryBucket(Arc<Mutex<MemoryState>>, Arc<AtomicUsize>, super::MemoryDb);
#[derive(Default)]
struct MemoryState {
    logs: BTreeMap<ProjectId, BTreeMap<OffsetDateTime, OperationLog>>,
    metadata: BTreeMap<(ProjectId, UserId), MemoryMetadataRecord>,
    objects: BTreeMap<String, Vec<u8>>,
    lists: BTreeMap<String, (usize, Vec<StoredFile>)>,
}
type MemoryMetadataRecord = (AuthorMetadata, BTreeMap<String, Vec<u8>>);
#[derive(Clone)]
struct StoredFile {
    key: String,
    extension: Option<String>,
}
impl StoredFile {
    fn extension(&self) -> Option<Extension<'_>> {
        self.extension
            .as_deref()
            .map(|value| Extension::new(value).expect("Stored file extensions must be valid"))
    }
}
pub struct MemoryFile<'a> {
    extension: Option<String>,
    source: MemorySource<'a>,
}
enum MemorySource<'a> {
    Stored { scope: String, key: String, revision: usize },
    New(BoxStream<'a, Result<Bytes, std::io::Error>>),
}
impl<'a> FileObject<'a> for MemoryFile<'a> {
    fn from_data(data: BoxStream<'a, Result<Bytes, std::io::Error>>, extension: Option<Extension<'_>>) -> Self {
        Self {
            extension: extension.map(|extension| extension.as_ref().to_owned()),
            source: MemorySource::New(data),
        }
    }
    fn extension(&self) -> Option<Extension<'_>> {
        self.extension
            .as_deref()
            .map(|value| Extension::new(value).expect("File extensions must be valid"))
    }
}
impl MemoryState {
    fn snapshot(&self, scope: &str) -> (usize, Vec<StoredFile>) {
        self.lists.get(scope).cloned().unwrap_or_default()
    }
}
#[derive(Clone)]
pub struct Object {
    bytes: Vec<u8>,
}
impl Object {
    pub fn size(&self) -> u64 {
        self.bytes.len() as u64
    }
    pub fn text(&self) -> String {
        String::from_utf8(self.bytes.clone()).unwrap()
    }
}
impl MemoryBucket {
    pub fn with_db(&self, db: super::MemoryDb) -> Self {
        Self(self.0.clone(), self.1.clone(), db)
    }
    pub fn started_submissions(&self) -> usize {
        self.1.load(Ordering::SeqCst)
    }
    // Fixture access, intentionally independent of the application-facing traits.
    pub async fn put(&self, key: impl Into<String>, data: impl Into<Vec<u8>>) -> Result<(), std::convert::Infallible> {
        self.0.lock().unwrap().objects.insert(key.into(), data.into());
        Ok(())
    }
    pub async fn get(&self, key: impl Into<String>) -> Result<Option<Object>, std::convert::Infallible> {
        let key = key.into();
        Ok(self.0.lock().unwrap().objects.get(&key).map(|data| Object { bytes: data.clone() }))
    }
    pub async fn head(&self, key: impl Into<String>) -> Result<Option<Object>, std::convert::Infallible> {
        self.get(key).await
    }
    pub async fn list(&self) -> Result<Vec<Object>, std::convert::Infallible> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .objects
            .values()
            .map(|bytes| Object { bytes: bytes.clone() })
            .collect())
    }
}
pub struct Project {
    bucket: MemoryBucket,
    prefix: String,
    project_id: ProjectId,
}
pub struct Scope {
    bucket: MemoryBucket,
    prefix: String,
    project_id: ProjectId,
}
pub struct UserResources {
    scope: Scope,
    username: Username,
}
pub struct Results {
    bucket: MemoryBucket,
    prefix: String,
    project_id: ProjectId,
}
pub struct UserResults {
    bucket: MemoryBucket,
    prefix: String,
    username: Username,
    project_id: ProjectId,
}

impl Bucket for MemoryBucket {
    type ProjectBucket<'a> = Project;
    fn project(&self, id: ProjectId) -> Project {
        Project {
            bucket: self.clone(),
            prefix: format!("projects/{id}/"),
            project_id: id,
        }
    }
}
impl ProjectBucket for Project {
    async fn append_log(&self, event: &OperationLog) -> Result<(), BucketError> {
        self.bucket
            .0
            .lock()
            .unwrap()
            .logs
            .entry(self.project_id)
            .or_default()
            .insert(OffsetDateTime::now_utc(), event.clone());
        Ok(())
    }

    async fn logs(&self, limit: u32, cursor: Option<&str>) -> Result<OperationLogPage, BucketError> {
        let before = cursor
            .map(|value| {
                let nanos = value.parse().map_err(|_| BucketError::InvalidKey)?;
                OffsetDateTime::from_unix_timestamp_nanos(nanos).map_err(|_| BucketError::InvalidKey)
            })
            .transpose()?;
        let state = self.bucket.0.lock().unwrap();
        let Some(logs) = state.logs.get(&self.project_id) else {
            return Ok(OperationLogPage {
                entries: Vec::new(),
                next_cursor: None,
            });
        };
        let mut logs = logs.range((Unbounded, before.map_or(Unbounded, Excluded))).rev();
        let entries: Vec<_> = logs
            .by_ref()
            .take(limit.clamp(1, 100) as usize)
            .map(|(&timestamp, event)| OperationLogEntry {
                timestamp,
                event: event.clone(),
            })
            .collect();
        let next_cursor = logs
            .next()
            .and_then(|_| entries.last().map(|entry| entry.timestamp.unix_timestamp_nanos().to_string()));
        Ok(OperationLogPage { entries, next_cursor })
    }

    type MetadataBucket<'a> = MemoryMetadata;
    fn metadata_by_user(&self, user_id: UserId) -> MemoryMetadata {
        MemoryMetadata {
            bucket: self.bucket.clone(),
            project: self.project_id,
            user: user_id,
        }
    }
    type ResourceBucket<'a> = Scope;
    type UserResourceBucket<'a> = UserResources;
    type ResultListBucket<'a> = Results;
    type UserResultBucket<'a> = UserResults;
    fn resource(&self) -> Scope {
        Scope {
            bucket: self.bucket.clone(),
            prefix: format!("{}resources/", self.prefix),
            project_id: self.project_id,
        }
    }
    fn resource_by_user(&self, username: Username) -> UserResources {
        UserResources {
            scope: Scope {
                bucket: self.bucket.clone(),
                prefix: format!("{}resources/authors/{username}/", self.prefix),
                project_id: self.project_id,
            },
            username,
        }
    }
    fn result(&self) -> Results {
        Results {
            bucket: self.bucket.clone(),
            prefix: format!("{}results/", self.prefix),
            project_id: self.project_id,
        }
    }
    fn result_by_user(&self, username: Username) -> UserResults {
        UserResults {
            bucket: self.bucket.clone(),
            prefix: format!("{}results/", self.prefix),
            username,
            project_id: self.project_id,
        }
    }
}
impl Scope {
    fn key(&self, key: &str) -> Result<String, BucketError> {
        if !valid_key(key) || self.prefix.len() + key.len() > 1024 {
            return Err(BucketError::InvalidKey);
        }
        Ok(format!("{}{key}", self.prefix))
    }
    async fn put(&self, key: &str, data: impl Stream<Item = Result<Bytes, std::io::Error>>) -> Result<(), BucketError> {
        let key = self.key(key)?;
        let bytes = data
            .try_fold(Vec::new(), |mut bytes, chunk| async move {
                bytes.extend_from_slice(&chunk);
                Ok(bytes)
            })
            .await
            .map_err(BucketError::Read)?;
        self.bucket.0.lock().unwrap().objects.insert(key, bytes);
        Ok(())
    }
    fn get(&self, key: &str) -> Result<Option<Data>, BucketError> {
        Ok(self.bucket.0.lock().unwrap().objects.get(&self.key(key)?).cloned().map(Data::from))
    }
}
impl ResourceBucket for Scope {
    fn as_s3(&self) -> impl s3s::S3 {
        UnusedS3
    }
    async fn download(&self, key: &str) -> Result<Option<Data>, BucketError> {
        let mut physical = self.key(key)?;
        if let Some(path) = key.strip_prefix("authors/") {
            let Some((author, name)) = path.split_once('/') else {
                return Ok(None);
            };
            let Some(member) = self
                .bucket
                .2
                .project(self.project_id)
                .all_member()
                .await?
                .into_iter()
                .find(|member| member.username().as_ref() == author)
            else {
                return Ok(None);
            };
            let state = self.bucket.0.lock().unwrap();
            let Some((_, files)) = state.lists.get(&format!("{}users/{}/", self.prefix, member.user_id())) else {
                return Ok(None);
            };
            let Some(file) = files.iter().enumerate().find(|(i, file)| resource_file_key(*i, file.extension()) == name) else {
                return Ok(None);
            };
            physical = file.1.key.clone();
        }
        Ok(self.bucket.0.lock().unwrap().objects.get(&physical).cloned().map(Data::from))
    }
}

pub struct MemoryMetadata {
    bucket: MemoryBucket,
    project: ProjectId,
    user: UserId,
}

impl AuthorMetadataBucket for MemoryMetadata {
    async fn load(&self) -> Result<AuthorMetadata, BucketError> {
        if self.bucket.2.project(self.project).find_member(self.user).await?.is_none() {
            return Err(BucketError::InvalidFile);
        }
        Ok(self
            .bucket
            .0
            .lock()
            .unwrap()
            .metadata
            .get(&(self.project, self.user))
            .map(|row| row.0.clone())
            .unwrap_or_default())
    }
    async fn save(&self, update: MetadataUpdate) -> Result<(), BucketError> {
        self.load().await?;
        update.validate().map_err(|e| BucketError::Read(std::io::Error::other(e)))?;
        let mut state = self.bucket.0.lock().unwrap();
        let (metadata, files) = state.metadata.entry((self.project, self.user)).or_default();
        for (key, change) in update.0 {
            match change {
                MetadataChange::String(value) => {
                    metadata.values.insert(key.clone(), value);
                    metadata.files.remove(&key);
                    files.remove(&key);
                }
                MetadataChange::File(file) => {
                    metadata.values.remove(&key);
                    metadata.files.insert(key.clone(), asset_filename(&key, file.filename()));
                    files.insert(key, file.bytes().to_vec());
                }
                MetadataChange::Remove => {
                    metadata.values.remove(&key);
                    metadata.files.remove(&key);
                    files.remove(&key);
                }
            }
        }
        Ok(())
    }
    async fn file(&self, key: &str) -> Result<Option<(String, Data)>, BucketError> {
        if !valid_field_key(key) {
            return Err(BucketError::InvalidKey);
        }
        self.load().await?;
        Ok(self
            .bucket
            .0
            .lock()
            .unwrap()
            .metadata
            .get(&(self.project, self.user))
            .and_then(|(metadata, files)| Some((metadata.files.get(key)?.clone(), files.get(key)?.clone().into()))))
    }
}
impl UserResources {
    fn identity_scope(&self, user_id: UserId) -> String {
        format!("projects/{}/resources/users/{user_id}/", self.scope.project_id)
    }
    async fn user_id(&self) -> Result<UserId, BucketError> {
        self.scope
            .bucket
            .2
            .project(self.scope.project_id)
            .all_member()
            .await?
            .into_iter()
            .find(|member| member.username() == &self.username)
            .map(|member| member.user_id())
            .ok_or(BucketError::InvalidFile)
    }
    async fn snapshot(&self) -> Result<(UserId, usize, Vec<StoredFile>), BucketError> {
        let user_id = self.user_id().await?;
        let state = self.scope.bucket.0.lock().unwrap();
        let (revision, files) = state.snapshot(&self.identity_scope(user_id));
        Ok((user_id, revision, files))
    }
}
impl UserResourceBucket for UserResources {
    type FileObject<'a> = MemoryFile<'a>;
    async fn current_file_list<'a>(&self) -> Result<Vec<MemoryFile<'a>>, BucketError> {
        let (user_id, revision, files) = self.snapshot().await?;
        if files.len() > MAX_RESOURCE_FILES {
            return Err(BucketError::TooManyFiles);
        }
        Ok(files
            .into_iter()
            .map(|file| MemoryFile {
                extension: file.extension,
                source: MemorySource::Stored {
                    scope: self.identity_scope(user_id),
                    key: file.key,
                    revision,
                },
            })
            .collect())
    }
    async fn update<'a>(&self, files: Vec<Self::FileObject<'a>>) -> Result<(), BucketError> {
        if files.len() > MAX_RESOURCE_FILES {
            return Err(BucketError::TooManyFiles);
        }
        let (user_id, revision, old) = self.snapshot().await?;
        let identity_scope = self.identity_scope(user_id);
        let mut retained = BTreeSet::new();
        for (index, file) in files.iter().enumerate() {
            self.scope.key(&resource_file_key(index, file.extension()))?;
            if let MemorySource::Stored {
                scope,
                key,
                revision: version,
            } = &file.source
            {
                if scope != &identity_scope || !retained.insert(key.clone()) {
                    return Err(BucketError::InvalidFile);
                }
                if *version != revision || !old.iter().any(|file| &file.key == key) {
                    return Err(BucketError::Conflict);
                }
            }
        }
        let mut entries = Vec::new();
        let mut created = Vec::new();
        for file in files {
            let key = match file.source {
                MemorySource::Stored { key, .. } => key,
                MemorySource::New(data) => {
                    let id = self.scope.bucket.1.fetch_add(1, Ordering::SeqCst);
                    let key = format!("projects/{}/resource-files/users/{user_id}/{id}", self.scope.project_id);
                    // Only the test backend collects contents in memory.
                    let bytes = data.map_ok(|bytes| bytes.to_vec()).try_concat().await.map_err(BucketError::Read)?;
                    created.push((key.clone(), bytes));
                    key
                }
            };
            entries.push(StoredFile {
                key,
                extension: file.extension,
            });
        }
        if self.user_id().await.ok() != Some(user_id) {
            return Err(BucketError::Conflict);
        }
        let mut state = self.scope.bucket.0.lock().unwrap();
        if state.snapshot(&identity_scope).0 != revision {
            return Err(BucketError::Conflict);
        }
        for file in old {
            if !retained.contains(&file.key) {
                state.objects.remove(&file.key);
            }
        }
        state.objects.extend(created);
        state.lists.insert(identity_scope, (revision + 1, entries));
        Ok(())
    }
}
impl ResultListBucket for Results {
    type ResultBucket<'a> = Scope;
    async fn list(&self) -> Result<Vec<BuildResult>, BucketError> {
        let mut runs = BTreeSet::new();
        for key in self.bucket.0.lock().unwrap().objects.keys() {
            let Some((run, _)) = key.strip_prefix(&self.prefix).and_then(|s| s.split_once('/')) else {
                continue;
            };
            if let Some(run) = ResultKey::new(run.into()) {
                runs.insert(run);
            }
        }
        Ok(runs.into_iter().map(BuildResult::from).collect())
    }
    fn result(&self, key: &ResultKey) -> Scope {
        Scope {
            bucket: self.bucket.clone(),
            prefix: format!("{}{}/", self.prefix, key.as_str()),
            project_id: self.project_id,
        }
    }
}
impl UserResultListBucket for UserResults {
    type ResultBucket<'a> = Scope;
    async fn list(&self) -> Result<Vec<BuildResult>, BucketError> {
        let author_prefix = format!("authors/{}/", self.username);
        let mut runs = BTreeSet::new();
        for key in self.bucket.0.lock().unwrap().objects.keys() {
            let Some((run, file)) = key.strip_prefix(&self.prefix).and_then(|s| s.split_once('/')) else {
                continue;
            };
            if file.strip_prefix(&author_prefix).is_some_and(valid_key)
                && let Some(run) = ResultKey::new(run.into())
            {
                runs.insert(run);
            }
        }
        Ok(runs.into_iter().map(BuildResult::from).collect())
    }
    fn result(&self, key: &ResultKey) -> Scope {
        Scope {
            bucket: self.bucket.clone(),
            prefix: format!("{}{}/authors/{}/", self.prefix, key.as_str(), self.username),
            project_id: self.project_id,
        }
    }
}
impl ResultBucket for Scope {
    fn as_s3(&self) -> impl s3s::S3 {
        UnusedS3
    }
    async fn list(&self) -> Result<Vec<File>, BucketError> {
        if self.prefix.len() > 1024 {
            return Err(BucketError::InvalidKey);
        }
        Ok(self
            .bucket
            .0
            .lock()
            .unwrap()
            .objects
            .iter()
            .rev()
            .filter_map(|(key, data)| {
                let key = key.strip_prefix(&self.prefix)?;
                valid_key(key).then_some(File {
                    key: key.into(),
                    size: data.len() as u64,
                })
            })
            .collect())
    }
    fn upload(
        &self,
        key: &str,
        data: impl Stream<Item = Result<Bytes, std::io::Error>> + Send,
    ) -> impl Future<Output = Result<(), BucketError>> + Send {
        self.put(key, data)
    }
    fn download(&self, key: &str) -> impl Future<Output = Result<Option<Data>, BucketError>> + Send {
        std::future::ready(self.get(key))
    }
}
impl UserResultBucket for Scope {}

// Application tests do not exercise S3; the protocol tests supply recording adapters.
struct UnusedS3;
impl s3s::S3 for UnusedS3 {}
