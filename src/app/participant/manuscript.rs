use crate::app::bucket_error;
use crate::auth::AuthState;
use crate::core::Config;
use crate::core::bucket::{
    Bucket, BucketError, Extension as FileExtension, FileObject, MAX_RESOURCE_FILES, ProjectBucket, ResourceBucket, UserResourceBucket,
    resource_file_key,
};
use crate::core::build::BuildRequests;
use crate::core::http::FileResponse;
use crate::core::operation_log::{OperationLog, record};
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use futures::{StreamExt, TryStreamExt};
use std::io;

pub(super) const MAX_MANIFEST_SIZE: usize = 64 * 1024;

fn extension(filename: &str) -> Result<Option<String>, BucketError> {
    if filename.is_empty() || filename.len() > 1024 {
        return Err(BucketError::InvalidKey);
    }
    let extension = extract_extension(filename).map(str::to_owned);
    if extension.as_deref().is_some_and(|ext| FileExtension::new(ext).is_none()) {
        return Err(BucketError::InvalidKey);
    }
    Ok(extension)
}

pub(super) fn extract_extension(filename: &str) -> Option<&str> {
    let filename = filename.rsplit('/').next().unwrap();
    filename
        .split_once('.')
        .filter(|(stem, extension)| !stem.is_empty() && !extension.trim().is_empty())
        .map(|(_, extension)| extension)
}

#[derive(serde::Deserialize)]
pub(super) struct UploadQuery {
    filename: String,
    revision: u64,
}

pub(super) async fn append<S: Config>(
    State(state): State<S>,
    Extension(auth): Extension<AuthState>,
    Query(query): Query<UploadQuery>,
    body: Body,
) -> Result<Response, (StatusCode, &'static str)> {
    if !auth.user.is_participant() {
        return Err((StatusCode::FORBIDDEN, "寄稿者権限が必要です。"));
    }
    let extension = extension(&query.filename).map_err(bucket_error)?;
    let bucket = state.bucket();
    let project = bucket.project(auth.project.id);
    let resources = project.resource_by_user(auth.user.username().clone());
    let snapshot = resources.current_snapshot().await.map_err(bucket_error)?;
    if query.revision != snapshot.revision {
        return Err(bucket_error(BucketError::Conflict));
    }
    if snapshot.files.len() >= MAX_RESOURCE_FILES {
        return Err(bucket_error(BucketError::TooManyFiles));
    }
    let data = body.into_data_stream().map_err(io::Error::other);
    let mut files = snapshot.files;
    files.push(FileObject::from_data(data.boxed(), extension.as_deref().and_then(FileExtension::new)));
    let names = files
        .iter()
        .enumerate()
        .map(|(index, file)| resource_file_key(index, file.extension()))
        .collect();
    resources.commit(query.revision, files).await.map_err(bucket_error)?;
    Ok(saved(&state, &auth, names, query.revision + 1).await)
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Manifest {
    revision: u64,
    pages: Vec<usize>,
}

pub(super) async fn edit<S: Config>(
    State(state): State<S>,
    Extension(auth): Extension<AuthState>,
    Json(manifest): Json<Manifest>,
) -> Result<Response, (StatusCode, &'static str)> {
    if !auth.user.is_participant() {
        return Err((StatusCode::FORBIDDEN, "寄稿者権限が必要です。"));
    }
    if manifest.pages.len() > MAX_RESOURCE_FILES {
        return Err(bucket_error(BucketError::TooManyFiles));
    }
    let bucket = state.bucket();
    let project = bucket.project(auth.project.id);
    let resources = project.resource_by_user(auth.user.username().clone());
    let snapshot = resources.current_snapshot().await.map_err(bucket_error)?;
    if manifest.revision != snapshot.revision {
        return Err(bucket_error(BucketError::Conflict));
    }
    let mut current: Vec<_> = snapshot.files.into_iter().map(Some).collect();
    let files = manifest
        .pages
        .into_iter()
        .map(|index| current.get_mut(index).and_then(Option::take).ok_or(BucketError::InvalidFile))
        .collect::<Result<Vec<_>, _>>()
        .map_err(bucket_error)?;
    let names = files
        .iter()
        .enumerate()
        .map(|(index, file)| resource_file_key(index, file.extension()))
        .collect();
    resources.commit(manifest.revision, files).await.map_err(bucket_error)?;
    Ok(saved(&state, &auth, names, manifest.revision + 1).await)
}

#[derive(serde::Deserialize)]
pub(super) struct SourceQuery {
    revision: u64,
}

pub(super) async fn source<S: Config>(
    State(state): State<S>,
    Extension(auth): Extension<AuthState>,
    Path(index): Path<usize>,
    Query(query): Query<SourceQuery>,
) -> Result<FileResponse, (StatusCode, &'static str)> {
    if !auth.user.is_participant() {
        return Err((StatusCode::FORBIDDEN, "寄稿者権限が必要です。"));
    }
    let bucket = state.bucket();
    let project = bucket.project(auth.project.id);
    let resources = project.resource_by_user(auth.user.username().clone());
    let snapshot = resources.current_snapshot().await.map_err(bucket_error)?;
    if query.revision != snapshot.revision {
        return Err(bucket_error(BucketError::Conflict));
    }
    let file = snapshot.files.get(index).ok_or((StatusCode::NOT_FOUND, "ファイルが見つかりません。"))?;
    let key = resource_file_key(index, file.extension());
    let data = project
        .resource()
        .download(&format!("authors/{}/{key}", auth.user.username()))
        .await
        .map_err(bucket_error)?
        .ok_or((StatusCode::NOT_FOUND, "ファイルが見つかりません。"))?;
    // Numbered paths can change while download resolves the object; do not return a different page.
    if resources.current_snapshot().await.map_err(bucket_error)?.revision != query.revision {
        return Err(bucket_error(BucketError::Conflict));
    }
    Ok(FileResponse {
        key,
        data,
        attachment: false,
    })
}

async fn saved<S: Config>(state: &S, auth: &AuthState, filenames: Vec<String>, revision: u64) -> Response {
    record(
        &state.bucket().project(auth.project.id),
        OperationLog::ManuscriptUpdated {
            user_id: auth.user.user_id(),
            username: auth.user.username().clone(),
            filename: if filenames.is_empty() {
                "原稿なし".into()
            } else {
                filenames.join(", ")
            },
        },
    )
    .await;
    let build = state.builds().request(auth.project.id).await;
    // The edit is already committed. A queue failure must not invite a duplicate upload.
    if let Err(error) = &build {
        tracing::error!(%error, "Manuscript saved but build request failed");
    }
    Json(serde_json::json!({ "revision": revision, "filenames": filenames, "build_requested": build.is_ok() })).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_extension() {
        assert_eq!(extract_extension("main.pdf"), Some("pdf"));
        assert_eq!(extract_extension("main"), None);
        assert_eq!(extract_extension("dir/main.pdf"), Some("pdf"));
        assert_eq!(extract_extension("dir.name/main.pdf"), Some("pdf"));
        assert_eq!(extract_extension("dir.name/main"), None);
        assert_eq!(extract_extension("dir/main.tar.gz"), Some("tar.gz"));
        assert_eq!(extract_extension("dir.name/main.tar.gz"), Some("tar.gz"));
        assert_eq!(extract_extension("dir1.name/dir2.name/main.pdf"), Some("pdf"));
    }
}
