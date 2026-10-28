use super::notice::Notices;
use super::{FilePath, RunPath, bucket_error, render, storage_error};
use crate::auth::AuthState;
use crate::core::Config;
use crate::core::bucket::{
    Bucket, BucketError, BuildResult, Extension as FileExtension, File, FileObject, ProjectBucket, ResultBucket, UserResourceBucket,
    UserResultListBucket,
};
use crate::core::build::BuildRequests;
use crate::core::http::FileResponse;
use crate::core::operation_log::{OperationLog, record};
use askama::Template;
use axum::extract::multipart::{MultipartError, MultipartRejection};
use axum::extract::{DefaultBodyLimit, Multipart, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Extension, Router};
use futures::StreamExt;
use smol_str::{SmolStr, format_smolstr};

mod metadata;

const INVALID_UPLOAD: (StatusCode, &str) = (StatusCode::BAD_REQUEST, "ファイルを1つ選択してください。");
const UPLOAD_TOO_LARGE: (StatusCode, &str) = (StatusCode::PAYLOAD_TOO_LARGE, "ファイルは256 MiB以下にしてください。");
const MAX_UPLOAD_SIZE: usize = 256 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
#[error("Manuscripts must not exceed 256 MiB")]
struct UploadTooLarge;

pub fn route<S>() -> Router<S>
where
    S: Config + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/participant", axum::routing::get(participant::<S>))
        .route(
            "/participant/metadata",
            axum::routing::get(metadata::edit::<S>)
                .post(metadata::save::<S>)
                .layer(DefaultBodyLimit::max(metadata::MAX_FORM_BYTES)),
        )
        .route("/participant/metadata/assets/{key}", axum::routing::get(metadata::file::<S>))
        .route(
            "/participant/upload",
            axum::routing::post(upload::<S>).layer(DefaultBodyLimit::max(MAX_UPLOAD_SIZE + 64 * 1024)),
        )
        .route("/participant/{run_id}", axum::routing::get(completed::<S>))
        .route("/participant/{run_id}/file/{*key}", axum::routing::get(file::<S>))
}

pub(super) const TITLE: &str = "原稿提出";

/// ```askama
/// <h1 class="h2 fw-bold mb-4">{{ TITLE }}</h1>
/// {% if uploaded %}<p class="alert alert-success" role="status">アップロードしました。</p>{% endif %}
/// <section class="card mb-4"><div class="card-body d-flex flex-wrap justify-content-between align-items-center gap-3">
///     <div><h2 class="h5">掲載情報</h2><p class="text-body-secondary mb-0">表記名やタイトルなど、掲載に必要な情報を入力します。</p></div>
///     <a class="btn btn-outline-primary" href="/{{ project_name }}/participant/metadata">{{ metadata::TITLE }}</a>
/// </div></section>
/// <section class="card mb-4"><div class="card-body">
/// <h2 class="h5 mb-3">原稿ファイルのアップロード</h2>
/// <form action="/{{ project_name }}/participant/upload" method="post" enctype="multipart/form-data">
///     <label class="form-label w-100">原稿ファイル <input class="form-control mt-2" type="file" name="file" required aria-describedby="upload-help" /></label>
///     <p class="form-text" id="upload-help">1ファイル256 MiBまでアップロードできます。</p>
///     <input class="btn btn-primary" type="submit" value="アップロード" />
/// </form>
/// </div></section>
/// <h2 class="h5 mb-3">{{ COMPLETED_TITLE }}</h2>
/// <ul class="list-group">
/// {% for run in runs %}
///     <li class="list-group-item p-3 text-break"><a href="/{{ project_name }}/participant/{{ run.key.as_str()|urlencode_strict }}">{{ run }}</a></li>
/// {% else %}
///     <li class="list-group-item p-4 text-body-secondary">完成稿はまだありません。</li>
/// {% endfor %}
/// </ul>
/// ```
#[derive(Debug, Template)]
#[template(ext = "html", in_doc = true)]
struct ParticipantTemplate<'a> {
    project_name: &'a str,
    uploaded: bool,
    runs: Vec<BuildResult>,
}

const COMPLETED_TITLE: &str = "完成稿確認";

/// ```askama
/// <h1 class="h2 fw-bold mb-4">{{ COMPLETED_TITLE }}</h1>
/// <h2 class="h5 mb-3 text-break">{{ run }}</h2>
/// <ul class="list-group">
/// {% for file in files %}
///     <li class="list-group-item d-flex flex-wrap justify-content-between align-items-center gap-3 p-3">
///         <div class="mw-100 text-break"><span class="fw-semibold">{{ file.key }}</span><small class="d-block text-body-secondary">{{ file.size }} bytes</small></div>
///         <div class="d-flex flex-wrap gap-2">
///         <a class="btn btn-outline-primary btn-sm" href="/{{ project_name }}/participant/{{ run.key.as_str()|urlencode_strict }}/file/{{ file.key|urlencode }}" target="_blank" rel="noopener">閲覧</a>
///         <a class="btn btn-outline-secondary btn-sm" href="/{{ project_name }}/participant/{{ run.key.as_str()|urlencode_strict }}/file/{{ file.key|urlencode }}?download=true">ダウンロード</a>
///         </div>
///     </li>
/// {% else %}<li class="list-group-item p-4 text-body-secondary">完成稿はまだありません。</li>{% endfor %}
/// </ul>
/// ```
#[derive(Debug, Template)]
#[template(ext = "html", in_doc = true)]
struct CompletedTemplate<'a> {
    project_name: &'a str,
    run: BuildResult,
    files: Vec<File>,
}

async fn participant<S: Config>(
    State(state): State<S>,
    Extension(auth_state): Extension<AuthState>,
    Extension(Notices(notices)): Extension<Notices>,
) -> Result<Response, (StatusCode, &'static str)> {
    if !auth_state.user.is_participant() {
        return Err((StatusCode::FORBIDDEN, "寄稿者権限が必要です。"));
    }
    let runs = state
        .bucket()
        .project(auth_state.project.id)
        .result_by_user(auth_state.user.username().clone())
        .list()
        .await
        .map_err(bucket_error)?;
    Ok(render(
        &auth_state,
        &[(super::root::TITLE, ""), (TITLE, "participant")],
        ParticipantTemplate {
            project_name: auth_state.project.name.as_ref(),
            uploaded: notices.get("uploaded").and_then(|value| value.as_bool()).unwrap_or_default(),
            runs,
        },
    ))
}

async fn completed<S: Config>(
    State(state): State<S>,
    Extension(auth_state): Extension<AuthState>,
    Path(RunPath { run_id }): Path<RunPath>,
) -> Result<Response, (StatusCode, &'static str)> {
    if !auth_state.user.is_participant() {
        return Err((StatusCode::FORBIDDEN, "寄稿者権限が必要です。"));
    }
    let files = state
        .bucket()
        .project(auth_state.project.id)
        .result_by_user(auth_state.user.username().clone())
        .result(&run_id)
        .list()
        .await
        .map_err(bucket_error)?;
    Ok(render(
        &auth_state,
        &[
            (super::root::TITLE, ""),
            (TITLE, "participant"),
            (COMPLETED_TITLE, &format!("participant/{}", run_id.as_str())),
        ],
        CompletedTemplate {
            project_name: auth_state.project.name.as_ref(),
            run: run_id.into(),
            files,
        },
    ))
}

#[derive(serde::Deserialize)]
struct DownloadQuery {
    #[serde(default)]
    download: bool,
}

async fn file<S: Config>(
    State(state): State<S>,
    Extension(auth_state): Extension<AuthState>,
    Path(FilePath { run_id, key }): Path<FilePath>,
    Query(query): Query<DownloadQuery>,
) -> Result<FileResponse, (StatusCode, &'static str)> {
    if !auth_state.user.is_participant() {
        return Err((StatusCode::FORBIDDEN, "寄稿者権限が必要です。"));
    }
    let data = state
        .bucket()
        .project(auth_state.project.id)
        .result_by_user(auth_state.user.username().clone())
        .result(&run_id)
        .download(&key)
        .await
        .map_err(bucket_error)?
        .ok_or((StatusCode::NOT_FOUND, "ファイルが見つかりません。"))?;
    Ok(FileResponse {
        key,
        data,
        attachment: query.download,
    })
}

async fn upload<S: Config>(
    State(state): State<S>,
    Extension(auth_state): Extension<AuthState>,
    multipart: Result<Multipart, MultipartRejection>,
) -> Result<Response, (StatusCode, &'static str)> {
    if !auth_state.user.is_participant() {
        return Err((StatusCode::FORBIDDEN, "寄稿者権限が必要です。"));
    }
    let mut multipart = multipart.map_err(|_| INVALID_UPLOAD)?;
    let bucket = state.bucket();
    let project = bucket.project(auth_state.project.id);
    let resources = project.resource_by_user(auth_state.user.username().clone());
    let filename = upload_file(&resources, &mut multipart).await?;
    record(
        &project,
        OperationLog::ManuscriptUpdated {
            user_id: auth_state.user.user_id(),
            username: auth_state.user.username().clone(),
            filename,
        },
    )
    .await;
    state.builds().request(auth_state.project.id).await.map_err(storage_error)?;
    Ok((
        Extension(Notices::from([("uploaded", true)])),
        Redirect::to(&format_smolstr!("/{}/participant", auth_state.project.name)),
    )
        .into_response())
}

fn extract_extension(filename: &str) -> Option<&str> {
    let filename = filename.rsplit('/').next().unwrap();
    filename
        .split_once('.')
        .filter(|(stem, extension)| !stem.is_empty() && !extension.trim().is_empty())
        .map(|(_, extension)| extension)
}

async fn upload_file<B: UserResourceBucket>(resources: &B, multipart: &mut Multipart) -> Result<String, (StatusCode, &'static str)> {
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|error| upload_error(BucketError::Read(std::io::Error::other(error))))?
    {
        if field.name() != Some("file") {
            continue;
        }
        let filename = field.file_name().filter(|name| !name.is_empty()).ok_or(INVALID_UPLOAD)?;
        let extension = extract_extension(filename).map(SmolStr::from);
        let extension = extension
            .as_deref()
            .map(|value| FileExtension::new(value).ok_or(BucketError::InvalidKey))
            .transpose()
            .map_err(upload_error)?;
        let filename = crate::core::bucket::resource_file_key(0, extension);
        let mut size = 0;
        let data = field.map(move |chunk| {
            let chunk = chunk.map_err(std::io::Error::other)?;
            size += chunk.len();
            if size > MAX_UPLOAD_SIZE {
                return Err(std::io::Error::other(UploadTooLarge));
            }
            Ok(chunk)
        });
        let file = B::FileObject::from_data(data.boxed(), extension);
        resources.update(vec![file]).await.map_err(upload_error)?;
        return Ok(filename);
    }
    Err(INVALID_UPLOAD)
}

fn upload_error(error: BucketError) -> (StatusCode, &'static str) {
    if let BucketError::Read(error) = &error
        && let Some(source) = error.get_ref()
    {
        if source.is::<UploadTooLarge>() {
            return UPLOAD_TOO_LARGE;
        }
        if let Some(error) = source.downcast_ref::<MultipartError>() {
            return match error.status() {
                StatusCode::PAYLOAD_TOO_LARGE => UPLOAD_TOO_LARGE,
                status => (status, "アップロードしたファイルを読み込めませんでした。"),
            };
        }
    }
    bucket_error(error)
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
