use super::{FilePath, RunPath, bucket_error, render};
use crate::auth::AuthState;
use crate::core::Config;
use crate::core::bucket::{Bucket, BuildResult, File, ProjectBucket, ResultBucket, ResultListBucket};
use crate::core::http::FileResponse;
use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::{Extension, Router};

pub fn route<S>() -> Router<S>
where
    S: Config + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/review", axum::routing::get(index::<S>))
        .route("/review/{run_id}", axum::routing::get(review::<S>))
        .route("/review/{run_id}/file/{*key}", axum::routing::get(file::<S>))
}

/// ```askama
/// {% if show_project_link %}<p class="mb-4"><a href="/{{ project_name }}/">プロジェクトトップに戻る</a></p>{% endif %}
/// <h1 class="h2 fw-bold mb-4">完成稿確認</h1>
/// <ul class="list-group">
/// {% for run in runs %}
///     <li class="list-group-item p-3 text-break"><a href="/{{ project_name }}/review/{{ run.key.as_str()|urlencode_strict }}">{{ run }}</a></li>
/// {% else %}<li class="list-group-item p-4 text-body-secondary">完成稿はまだありません。</li>{% endfor %}
/// </ul>
/// ```
#[derive(Debug, Template)]
#[template(ext = "html", in_doc = true)]
struct ReviewIndexTemplate<'a> {
    project_name: &'a str,
    show_project_link: bool,
    runs: &'a [BuildResult],
}

/// ```askama
/// <p class="mb-4"><a href="/{{ project_name }}/review">一覧に戻る</a></p>
/// <h1 class="h2 fw-bold mb-4">完成稿確認</h1>
/// <h2 class="h5 mb-3 text-break">{{ run }}</h2>
/// <ul class="list-group">
/// {% for file in files %}
///     <li class="list-group-item d-flex flex-wrap justify-content-between align-items-center gap-3 p-3">
///         <div class="mw-100 text-break"><span class="fw-semibold">{{ file.key }}</span><small class="d-block text-body-secondary">{{ file.size }} bytes</small></div>
///         <div class="d-flex flex-wrap gap-2">
///         <a class="btn btn-outline-primary btn-sm" href="/{{ project_name }}/review/{{ run.key.as_str()|urlencode_strict }}/file/{{ file.key|urlencode }}" target="_blank" rel="noopener">閲覧</a>
///         <a class="btn btn-outline-secondary btn-sm" href="/{{ project_name }}/review/{{ run.key.as_str()|urlencode_strict }}/file/{{ file.key|urlencode }}?download=true">ダウンロード</a>
///         </div>
///     </li>
/// {% else %}<li class="list-group-item p-4 text-body-secondary">完成稿はまだありません。</li>{% endfor %}
/// </ul>
/// ```
#[derive(Debug, Template)]
#[template(ext = "html", in_doc = true)]
struct ReviewTemplate<'a> {
    project_name: &'a str,
    run: BuildResult,
    files: Vec<File>,
}

async fn index<S: Config>(State(state): State<S>, Extension(auth_state): Extension<AuthState>) -> Result<Response, (StatusCode, &'static str)> {
    if !auth_state.user.is_reviewer() {
        return Err((StatusCode::FORBIDDEN, "レビュワー権限が必要です。"));
    }
    let runs = state
        .bucket()
        .project(auth_state.project.id)
        .result()
        .list()
        .await
        .map_err(bucket_error)?;
    Ok(render(
        "完成稿確認",
        ReviewIndexTemplate {
            project_name: auth_state.project.name.as_ref(),
            show_project_link: auth_state.user.permissions().bits().count_ones() >= 2,
            runs: &runs,
        },
    ))
}

async fn review<S: Config>(
    State(state): State<S>,
    Extension(auth_state): Extension<AuthState>,
    Path(RunPath { run_id }): Path<RunPath>,
) -> Result<Response, (StatusCode, &'static str)> {
    if !auth_state.user.is_reviewer() {
        return Err((StatusCode::FORBIDDEN, "レビュワー権限が必要です。"));
    }
    let files = state
        .bucket()
        .project(auth_state.project.id)
        .result()
        .result(&run_id)
        .list()
        .await
        .map_err(bucket_error)?;
    Ok(render(
        "完成稿確認",
        ReviewTemplate {
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
    if !auth_state.user.is_reviewer() {
        return Err((StatusCode::FORBIDDEN, "レビュワー権限が必要です。"));
    }
    let data = state
        .bucket()
        .project(auth_state.project.id)
        .result()
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
