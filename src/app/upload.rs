use super::storage_error;
use crate::core::Config;
use crate::core::environment::{EnvironmentUpload, EnvironmentUploads};
use crate::core::project::ProjectStore;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response;
use axum::{Json, Router};

pub fn route<S>() -> Router<S>
where
    S: Config + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/{key}", axum::routing::get(presign::<S>))
        .layer(axum::middleware::map_response(|mut response: Response| async move {
            response
                .headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("private, no-store"));
            response
        }))
}

async fn presign<S: Config>(State(state): State<S>, Path(key): Path<String>) -> Result<Json<EnvironmentUpload>, (StatusCode, &'static str)> {
    let project = state
        .projects()
        .find_environment_upload_project(&key)
        .await
        .map_err(storage_error)?
        .ok_or((StatusCode::NOT_FOUND, "アップロードURLが無効です。"))?;
    let upload = state.environment_uploads().presign(project).map_err(storage_error)?;
    Ok(Json(upload))
}
