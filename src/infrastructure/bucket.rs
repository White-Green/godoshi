use super::s3::S3Bucket;
use crate::core::bucket::*;
use crate::core::project::ProjectId;
use crate::core::user::Username;
use bytes::Bytes;
use futures::{Stream, StreamExt, TryStreamExt};
use std::collections::BTreeSet;
use std::sync::Arc;
use time::OffsetDateTime;

pub(crate) mod resources;
mod upload;

#[derive(Clone)]
pub struct R2Bucket {
    pub(super) bucket: worker::Bucket,
    db: Arc<worker::D1Database>,
}
impl R2Bucket {
    pub fn new(bucket: worker::Bucket, db: worker::D1Database) -> Self {
        Self { bucket, db: Arc::new(db) }
    }
}

pub struct R2Project<'a> {
    bucket: &'a worker::Bucket,
    db: &'a Arc<worker::D1Database>,
    prefix: String,
    project_id: ProjectId,
}
struct R2Scope<'a> {
    bucket: &'a worker::Bucket,
    db: &'a Arc<worker::D1Database>,
    prefix: String,
}
pub struct R2Resource<'a> {
    scope: R2Scope<'a>,
    project_id: ProjectId,
}
pub struct R2UserResources<'a> {
    scope: R2Scope<'a>,
    project_id: ProjectId,
    username: Username,
}
pub struct R2Result<'a>(R2Scope<'a>);
pub struct R2Results<'a> {
    bucket: &'a worker::Bucket,
    db: &'a Arc<worker::D1Database>,
    prefix: String,
}
pub struct R2UserResults<'a> {
    bucket: &'a worker::Bucket,
    db: &'a Arc<worker::D1Database>,
    prefix: String,
    username: Username,
}

fn error(error: worker::Error) -> BucketError {
    std::io::Error::other(error.to_string()).into()
}

impl Bucket for R2Bucket {
    type ProjectBucket<'a> = R2Project<'a>;
    fn project(&self, project_id: ProjectId) -> Self::ProjectBucket<'_> {
        R2Project {
            bucket: &self.bucket,
            db: &self.db,
            prefix: format!("projects/{project_id}/"),
            project_id,
        }
    }
}

impl ProjectBucket for R2Project<'_> {
    type ResourceBucket<'a>
        = R2Resource<'a>
    where
        Self: 'a;
    type UserResourceBucket<'a>
        = R2UserResources<'a>
    where
        Self: 'a;
    type ResultListBucket<'a>
        = R2Results<'a>
    where
        Self: 'a;
    type UserResultBucket<'a>
        = R2UserResults<'a>
    where
        Self: 'a;
    fn resource(&self) -> R2Resource<'_> {
        R2Resource {
            scope: R2Scope {
                bucket: self.bucket,
                db: self.db,
                prefix: format!("{}resources/", self.prefix),
            },
            project_id: self.project_id,
        }
    }
    fn resource_by_user(&self, username: Username) -> R2UserResources<'_> {
        R2UserResources {
            scope: R2Scope {
                bucket: self.bucket,
                db: self.db,
                prefix: format!("{}resources/authors/{username}/", self.prefix),
            },
            project_id: self.project_id,
            username,
        }
    }
    fn result(&self) -> R2Results<'_> {
        R2Results {
            bucket: self.bucket,
            db: self.db,
            prefix: format!("{}results/", self.prefix),
        }
    }
    fn result_by_user(&self, username: Username) -> R2UserResults<'_> {
        R2UserResults {
            bucket: self.bucket,
            db: self.db,
            prefix: format!("{}results/", self.prefix),
            username,
        }
    }
}

impl R2Scope<'_> {
    fn key(&self, key: &str) -> Result<String, BucketError> {
        if !valid_key(key) || self.prefix.len() + key.len() > 1024 {
            return Err(BucketError::InvalidKey);
        }
        Ok(format!("{}{key}", self.prefix))
    }
    fn put(&self, key: &str, data: impl Stream<Item = Result<Bytes, std::io::Error>> + Send) -> impl Future<Output = Result<(), BucketError>> + Send {
        upload::put(self, key, data)
    }
    #[worker::send]
    async fn get(&self, key: &str) -> Result<Option<Data>, BucketError> {
        self.get_physical(&self.key(key)?).await
    }
    #[worker::send]
    async fn get_physical(&self, key: &str) -> Result<Option<Data>, BucketError> {
        let Some(object) = self.bucket.get(key).execute().await.map_err(error)? else {
            return Ok(None);
        };
        let size = object.size();
        let body = object
            .body()
            .expect("Unconditional R2 GET must have a body")
            .response_body()
            .map_err(error)?;
        let response: axum::http::Response<axum::body::Body> = worker::Response::from_body(body).map_err(error)?.into();
        let stream = response.into_body().into_data_stream().map_err(std::io::Error::other).boxed();
        Ok(Some(Data { size, stream }))
    }
}

impl ResourceBucket for R2Resource<'_> {
    fn as_s3(&self) -> impl s3s::S3 {
        S3Bucket::resources(self.scope.bucket.clone(), self.scope.db.clone(), self.project_id)
    }
    fn upload_environment(
        &self,
        data: impl Stream<Item = Result<Bytes, std::io::Error>> + Send,
    ) -> impl Future<Output = Result<(), BucketError>> + Send {
        self.scope.put("environment.closure_zstd", data)
    }
    #[worker::send]
    async fn download(&self, key: &str) -> Result<Option<Data>, BucketError> {
        self.scope.key(key)?;
        let Some(key) = resources::resolve(self.scope.db, self.project_id, key).await? else {
            return Ok(None);
        };
        self.scope.get_physical(&key).await
    }
}

impl ResultListBucket for R2Results<'_> {
    type ResultBucket<'a>
        = R2Result<'a>
    where
        Self: 'a;
    fn generate_key(&self, now: OffsetDateTime) -> Result<ResultKey, BucketError> {
        let mut random = [0; 8];
        getrandom::fill(&mut random).map_err(std::io::Error::other)?;
        Ok(ResultKey::for_build(now, u64::from_le_bytes(random)))
    }
    #[worker::send]
    async fn list(&self) -> Result<Vec<BuildResult>, BucketError> {
        let mut cursor = None;
        let mut runs = BTreeSet::new();
        loop {
            let mut list = self.bucket.list().prefix(&self.prefix).delimiter("/").limit(100);
            if let Some(cursor) = cursor.take() {
                list = list.cursor(cursor);
            }
            let objects = list.execute().await.map_err(error)?;
            runs.extend(
                objects
                    .delimited_prefixes()
                    .into_iter()
                    .filter_map(|key| ResultKey::new(key.strip_prefix(&self.prefix)?.trim_end_matches('/').to_owned())),
            );
            if !objects.truncated() {
                break;
            }
            cursor = Some(objects.cursor().expect("Truncated R2 listing must include a cursor"));
        }
        Ok(runs.into_iter().map(BuildResult::from).collect())
    }
    fn result(&self, key: &ResultKey) -> R2Result<'_> {
        R2Result(R2Scope {
            bucket: self.bucket,
            db: self.db,
            prefix: format!("{}{}/", self.prefix, key.as_str()),
        })
    }
}

impl UserResultListBucket for R2UserResults<'_> {
    type ResultBucket<'a>
        = R2Result<'a>
    where
        Self: 'a;
    #[worker::send]
    async fn list(&self) -> Result<Vec<BuildResult>, BucketError> {
        let author_prefix = format!("authors/{}/", self.username);
        let mut cursor = None;
        let mut runs = BTreeSet::new();
        loop {
            let mut list = self.bucket.list().prefix(&self.prefix).limit(100);
            if let Some(cursor) = cursor.take() {
                list = list.cursor(cursor);
            }
            let objects = list.execute().await.map_err(error)?;
            for object in objects.objects() {
                let key = object.key();
                let Some((run, path)) = key.strip_prefix(&self.prefix).and_then(|key| key.split_once('/')) else {
                    continue;
                };
                if path.strip_prefix(&author_prefix).is_some_and(valid_key)
                    && let Some(run) = ResultKey::new(run.to_owned())
                {
                    runs.insert(run);
                }
            }
            if !objects.truncated() {
                break;
            }
            cursor = Some(objects.cursor().expect("Truncated R2 listing must include a cursor"));
        }
        Ok(runs.into_iter().map(BuildResult::from).collect())
    }
    fn result(&self, key: &ResultKey) -> R2Result<'_> {
        R2Result(R2Scope {
            bucket: self.bucket,
            db: self.db,
            prefix: format!("{}{}/authors/{}/", self.prefix, key.as_str(), self.username),
        })
    }
}

impl ResultBucket for R2Result<'_> {
    fn as_s3(&self) -> impl s3s::S3 {
        S3Bucket::results(self.0.bucket.clone(), self.0.db.clone(), self.0.prefix.clone())
    }
    #[worker::send]
    async fn list(&self) -> Result<Vec<File>, BucketError> {
        if self.0.prefix.len() > 1024 {
            return Err(BucketError::InvalidKey);
        }
        let mut files = Vec::new();
        let mut cursor = None;
        loop {
            let mut list = self.0.bucket.list().prefix(&self.0.prefix).limit(100);
            if let Some(cursor) = cursor.take() {
                list = list.cursor(cursor);
            }
            let objects = list.execute().await.map_err(error)?;
            files.extend(objects.objects().into_iter().filter_map(|object| {
                let key = object.key().strip_prefix(&self.0.prefix)?.to_owned();
                valid_key(&key).then_some(File { key, size: object.size() })
            }));
            if !objects.truncated() {
                break;
            }
            cursor = Some(objects.cursor().expect("Truncated R2 listing must include a cursor"));
        }
        files.sort_by(|a, b| b.key.cmp(&a.key));
        Ok(files)
    }
    fn upload(
        &self,
        key: &str,
        data: impl Stream<Item = Result<Bytes, std::io::Error>> + Send,
    ) -> impl Future<Output = Result<(), BucketError>> + Send {
        self.0.put(key, data)
    }
    fn download(&self, key: &str) -> impl Future<Output = Result<Option<Data>, BucketError>> + Send {
        self.0.get(key)
    }
}
impl UserResultBucket for R2Result<'_> {}

#[cfg(feature = "worker-tests")]
pub mod tests;
