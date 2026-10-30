use super::{FilePath, RunPath, bucket_error, render};
use crate::auth::AuthState;
use crate::core::Config;
use crate::core::bucket::{
    Bucket, BuildResult, File, FileObject, MAX_RESOURCE_FILES, ProjectBucket, ResourceSnapshot, ResultBucket, UserResourceBucket,
    UserResultListBucket, resource_file_key,
};
use crate::core::http::FileResponse;
use askama::Template;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::{Extension, Router};
use futures::TryFutureExt;

mod manuscript;
mod metadata;

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
        .route("/participant/files", axum::routing::post(manuscript::append::<S>))
        .route(
            "/participant/manuscript",
            axum::routing::post(manuscript::edit::<S>).layer(DefaultBodyLimit::max(manuscript::MAX_MANIFEST_SIZE)),
        )
        .route("/participant/source/{index}", axum::routing::get(manuscript::source::<S>))
        .route("/participant/{run_id}", axum::routing::get(completed::<S>))
        .route("/participant/{run_id}/file/{*key}", axum::routing::get(file::<S>))
}

pub(super) const TITLE: &str = "原稿提出";

/// ```askama
/// <h1 class="h2 fw-bold mb-4">{{ TITLE }}</h1>
/// <link rel="stylesheet" href="/static/manuscript-upload.css" />
/// <section class="card mb-4" aria-labelledby="manuscript-title"><div class="card-body">
/// <h2 class="h5 mb-3" id="manuscript-title">原稿ファイル</h2>
/// <form data-manuscript-upload data-revision="{{ revision }}" data-max-files="{{ MAX_RESOURCE_FILES }}" data-files-url="/{{ project_name }}/participant/files" data-commit-url="/{{ project_name }}/participant/manuscript" data-source-url="/{{ project_name }}/participant/source">
///     <button class="btn btn-outline-primary w-100 p-4 mb-2" type="button" data-add-pages aria-label="原稿ファイルを追加">
///         <span class="d-block fw-semibold">クリックしてファイルを選択</span>
///         <span class="d-block small mt-1">または、ここにドラッグ＆ドロップ</span>
///     </button>
///     <input type="file" multiple data-add-files hidden aria-label="追加する原稿ファイル" />
///     <div class="d-flex flex-wrap justify-content-between align-items-baseline gap-2 mb-2">
///         <p class="fw-semibold mb-0" data-page-summary aria-live="polite"></p>
///         <p class="small text-body-secondary text-break mb-0" data-edit-status role="status"></p>
///     </div>
///     <p class="alert alert-danger d-none" data-upload-error role="alert"></p>
///     <div class="d-flex flex-wrap gap-2 mb-3 d-none" data-upload-recovery>
///         <button class="btn btn-outline-primary" type="button" data-retry-uploads>未送信のファイルを再送信</button>
///         <button class="btn btn-outline-secondary" type="button" data-cancel-uploads>残りの追加を中止</button>
///     </div>
///     <a class="btn btn-outline-secondary mb-3 d-none" href="/{{ project_name }}/participant" data-reload-manuscript>最新の状態を読み込む</a>
///     <ol class="list-unstyled row row-cols-1 row-cols-md-2 row-cols-lg-3 row-cols-xl-4 g-3 mb-3" data-page-list aria-label="提出済みの原稿">
///     {% for name in submitted %}
///         <li class="col text-break" data-existing-page data-index="{{ loop.index0 }}" data-name="{{ name }}" data-url="/{{ project_name }}/participant/source/{{ loop.index0 }}?revision={{ revision }}">{{ loop.index }}ページ目</li>
///     {% endfor %}
///     </ol>
///     <template data-page-template>
///         <li class="col">
///             <div class="card h-100 p-3"><div class="row g-3 h-100">
///                 <div class="col-5 col-md-12">
///                     <div class="ratio manuscript-thumbnail">
///                         <button class="btn btn-outline-secondary p-1" type="button" data-preview-page><img class="w-100 h-100 object-fit-contain" loading="lazy" decoding="async" alt="" /></button>
///                         <div class="d-flex flex-column justify-content-center border rounded bg-body-tertiary p-2 small text-body-secondary text-center text-break d-none" data-preview-fallback>
///                             <p class="mb-2">プレビューなし</p><a target="_blank" rel="noopener">ファイルを開く</a>
///                         </div>
///                     </div>
///                 </div>
///                 <div class="col-7 col-md-12 d-flex flex-column">
///                     <div class="text-break mb-3">
///                         <strong></strong>
///                         <span class="badge text-bg-light border mt-2 d-none"></span>
///                     </div>
///                     <div class="d-flex flex-wrap gap-2 mt-auto">
///                         <button class="btn btn-sm btn-outline-secondary" type="button" data-action="up">前へ</button>
///                         <button class="btn btn-sm btn-outline-secondary" type="button" data-action="down">次へ</button>
///                         <button class="btn btn-sm btn-outline-danger" type="button" data-action="remove">削除</button>
///                     </div>
///                 </div>
///             </div></div>
///         </li>
///     </template>
///     <p class="text-body-secondary mb-0 d-none" data-empty-pages>原稿はまだ提出されていません。</p>
///     <noscript><p class="alert alert-warning">原稿の追加・編集にはJavaScriptを有効にしてください。</p></noscript>
///     <dialog class="manuscript-dialog p-3 border-0 rounded-3" data-preview-dialog aria-labelledby="preview-title">
///         <div class="d-flex justify-content-between align-items-center gap-3 mb-3"><h3 class="h5 mb-0" id="preview-title" data-preview-title></h3><button class="btn btn-outline-secondary btn-sm" type="button" data-close-preview>閉じる</button></div>
///         <img class="manuscript-full-image d-block w-100 object-fit-contain" data-preview-image alt="" />
///         <p class="small text-body-secondary text-break mt-3 mb-0" data-preview-name></p>
///     </dialog>
/// </form>
/// </div></section>
/// <section class="card mb-4"><div class="card-body d-flex flex-wrap justify-content-between align-items-center gap-3">
///     <div><h2 class="h5">掲載情報</h2><p class="text-body-secondary mb-0">表記名や作品タイトルなどを入力します。</p></div>
///     <a class="btn btn-outline-secondary" href="/{{ project_name }}/participant/metadata">{{ metadata::TITLE }}</a>
/// </div></section>
/// <script src="/static/manuscript-upload.js" defer></script>
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
    submitted: Vec<String>,
    revision: u64,
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

async fn participant<S: Config>(State(state): State<S>, Extension(auth_state): Extension<AuthState>) -> Result<Response, (StatusCode, &'static str)> {
    if !auth_state.user.is_participant() {
        return Err((StatusCode::FORBIDDEN, "寄稿者権限が必要です。"));
    }
    let bucket = state.bucket();
    let project = bucket.project(auth_state.project.id);
    let resource = project.resource_by_user(auth_state.user.username().clone());
    let result = project.result_by_user(auth_state.user.username().clone());
    let (ResourceSnapshot { revision, files }, runs) =
        futures::try_join!(resource.current_snapshot().map_err(bucket_error), result.list().map_err(bucket_error))?;
    let submitted = files
        .iter()
        .enumerate()
        .map(|(index, file)| resource_file_key(index, file.extension()))
        .collect();
    Ok(render(
        &auth_state,
        &[(super::root::TITLE, ""), (TITLE, "participant")],
        ParticipantTemplate {
            project_name: auth_state.project.name.as_ref(),
            submitted,
            revision,
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
