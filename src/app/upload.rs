use super::{bucket_error, storage_error};
use crate::core::Config;
use crate::core::bucket::{Bucket, ProjectBucket, ResourceBucket};
use crate::core::project::ProjectStore;
use axum::Router;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use futures::TryStreamExt;

pub fn route<S>() -> Router<S>
where
    S: Config + Clone + Send + Sync + 'static,
{
    Router::new().route("/{key}", axum::routing::put(upload::<S>))
}

async fn upload<S: Config>(State(state): State<S>, Path(key): Path<String>, body: Body) -> Result<StatusCode, (StatusCode, &'static str)> {
    let project = state
        .projects()
        .find_environment_upload_project(&key)
        .await
        .map_err(storage_error)?
        .ok_or((StatusCode::NOT_FOUND, "アップロードURLが無効です。"))?;
    state
        .bucket()
        .project(project)
        .resource()
        .upload_environment(body.into_data_stream().map_err(std::io::Error::other))
        .await
        .map_err(bucket_error)?;
    Ok(StatusCode::NO_CONTENT)
}
