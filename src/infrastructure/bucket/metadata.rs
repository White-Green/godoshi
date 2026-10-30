use super::{R2Scope, error};
use crate::core::bucket::{BucketError, Data};
use crate::core::metadata::*;
use crate::core::project::ProjectId;
use crate::core::user::UserId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use worker::{Bucket, D1Database};

pub struct R2AuthorMetadata<'a> {
    pub(super) bucket: &'a Bucket,
    pub(super) db: &'a std::sync::Arc<D1Database>,
    pub(super) project_id: ProjectId,
    pub(super) user_id: UserId,
}

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct Asset {
    pub key: String,
    pub filename: String,
}

#[derive(Deserialize)]
pub(crate) struct StoredMetadata {
    pub values_json: String,
    pub assets: String,
    pub updated_at: i64,
}

impl StoredMetadata {
    pub fn values(&self) -> Result<BTreeMap<String, String>, BucketError> {
        serde_json::from_str(&self.values_json).map_err(|e| std::io::Error::other(e).into())
    }
    pub fn assets(&self) -> Result<BTreeMap<String, Asset>, BucketError> {
        serde_json::from_str(&self.assets).map_err(|e| std::io::Error::other(e).into())
    }
    pub fn document(&self) -> Result<Vec<u8>, BucketError> {
        let mut values = self.values()?;
        for (key, asset) in self.assets()? {
            values.insert(key, asset_path(&asset.filename));
        }
        serde_json::to_vec(&values).map_err(|e| std::io::Error::other(e).into())
    }
    pub fn public(&self) -> Result<AuthorMetadata, BucketError> {
        Ok(AuthorMetadata {
            values: self.values()?,
            files: self.assets()?.into_iter().map(|(key, asset)| (key, asset.filename)).collect(),
        })
    }
}

impl R2AuthorMetadata<'_> {
    async fn stored(&self) -> Result<StoredMetadata, BucketError> {
        worker::query!(
            self.db,
            "SELECT COALESCE(a.values_json, '{}') AS values_json, COALESCE(a.assets, '{}') AS assets,
                    COALESCE(a.updated_at, 0) AS updated_at
             FROM project_member m LEFT JOIN author_metadata a ON a.project_id = m.project_id AND a.user_id = m.user_id
             WHERE m.project_id = ? AND m.user_id = ?",
            self.project_id,
            self.user_id
        )
        .map_err(error)?
        .first(None)
        .await
        .map_err(error)?
        .ok_or(BucketError::InvalidFile)
    }
}

impl AuthorMetadataBucket for R2AuthorMetadata<'_> {
    #[worker::send]
    async fn load(&self) -> Result<AuthorMetadata, BucketError> {
        self.stored().await?.public()
    }

    #[worker::send]
    async fn save(&self, update: MetadataUpdate) -> Result<(), BucketError> {
        update.validate().map_err(|e| BucketError::Read(std::io::Error::other(e)))?;
        let old = self.stored().await?;
        let mut values = old.values()?;
        let mut assets = old.assets()?;
        // No schema checks or optimistic locking. Each D1 write and R2 PUT is last-write-wins;
        // they are not an atomic operation and a D1 failure does not roll back an R2 overwrite.
        for (field, change) in update.0 {
            match change {
                MetadataChange::String(text) => {
                    values.insert(field.clone(), text);
                    assets.remove(&field);
                }
                MetadataChange::Remove => {
                    values.remove(&field);
                    assets.remove(&field);
                }
                MetadataChange::File(file) => {
                    let key = format!("projects/{}/metadata-assets/users/{}/{field}", self.project_id, self.user_id);
                    let filename = asset_filename(&field, file.filename());
                    self.bucket
                        .put(&key, file.bytes().to_vec())
                        .http_metadata(worker::HttpMetadata {
                            content_type: Some("application/octet-stream".into()),
                            ..Default::default()
                        })
                        .execute()
                        .await
                        .map_err(error)?;
                    values.remove(&field);
                    assets.insert(field, Asset { key, filename });
                }
            }
        }
        // Bound accumulated values as well as each request, including fields removed from the form.
        if values.values().map(String::len).sum::<usize>() > MAX_TEXT_BYTES || values.len() + assets.len() > MAX_FIELDS * 4 {
            return Err(BucketError::Read(std::io::Error::other("Stored metadata is too large")));
        }
        let values = serde_json::to_string(&values).map_err(std::io::Error::other)?;
        let assets = serde_json::to_string(&assets).map_err(std::io::Error::other)?;
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        worker::query!(
            self.db,
            "INSERT INTO author_metadata (project_id, user_id, values_json, assets, updated_at) VALUES (?, ?, ?, ?, ?)
             ON CONFLICT (project_id, user_id) DO UPDATE SET values_json = excluded.values_json,
                 assets = excluded.assets, updated_at = excluded.updated_at",
            self.project_id,
            self.user_id,
            values,
            assets,
            now
        )
        .map_err(error)?
        .run()
        .await
        .map_err(error)?;
        // Removed fields may leave their fixed R2 object behind. No cleanup or rollback is required.
        Ok(())
    }

    #[worker::send]
    async fn file(&self, key: &str) -> Result<Option<(String, Data)>, BucketError> {
        if !valid_field_key(key) {
            return Err(BucketError::InvalidKey);
        }
        let Some(asset) = self.stored().await?.assets()?.remove(key) else {
            return Ok(None);
        };
        let scope = R2Scope {
            bucket: self.bucket,
            db: self.db,
            prefix: String::new(),
        };
        Ok(scope.get_physical(&asset.key).await?.map(|data| (asset.filename, data)))
    }
}

pub(crate) async fn find(db: &D1Database, project: ProjectId, username: &str) -> Result<Option<StoredMetadata>, BucketError> {
    worker::query!(
        db,
        "SELECT COALESCE(a.values_json, '{}') AS values_json, COALESCE(a.assets, '{}') AS assets,
                COALESCE(a.updated_at, 0) AS updated_at
         FROM project_member m LEFT JOIN author_metadata a ON a.project_id = m.project_id AND a.user_id = m.user_id
         WHERE m.project_id = ? AND m.username = ?",
        project,
        username
    )
    .map_err(error)?
    .first(None)
    .await
    .map_err(error)
}
