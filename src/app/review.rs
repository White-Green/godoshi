use super::render;
use super::results::{self, FileQuery, ResultFile, ResultRun, RunPath};
use crate::auth::AuthState;
use crate::config::GenericConfigProvider;
use crate::user::UserPermissionSet;
use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::{Extension, Router};

pub fn route<S>() -> Router<S>
where
    S: GenericConfigProvider + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/review", axum::routing::get(index::<S>))
        .route("/review/{run_id}", axum::routing::get(review::<S>))
        .route("/review/{run_id}/file", axum::routing::get(file::<S>))
}

/// ```askama
/// <p><a href="/{{ project_name }}/">戻る</a></p>
/// <h1>完成稿確認</h1>
/// <ul>
/// {% for run in runs %}
///     <li><a href="/{{ project_name }}/review/{{ run.key }}">{{ run.name }}</a></li>
/// {% else %}<li>完成稿はまだありません。</li>{% endfor %}
/// </ul>
/// ```
#[derive(Debug, Template)]
#[template(ext = "html", in_doc = true)]
struct ReviewIndexTemplate<'a> {
    project_name: &'a str,
    runs: &'a [ResultRun],
}

/// ```askama
/// <p><a href="/{{ project_name }}/review">一覧に戻る</a></p>
/// <h1>完成稿確認</h1>
/// <h2>{{ run.name }}</h2>
/// <ul>
/// {% for file in files %}
///     <li>{{ file.name }} ({{ file.size }} bytes)
///         <a href="/{{ project_name }}/review/{{ run.key }}/file?key={{ file.key }}" target="_blank" rel="noopener">閲覧</a>
///         <a href="/{{ project_name }}/review/{{ run.key }}/file?key={{ file.key }}&amp;download=true">ダウンロード</a>
///     </li>
/// {% else %}<li>完成稿はまだありません。</li>{% endfor %}
/// </ul>
/// ```
#[derive(Debug, Template)]
#[template(ext = "html", in_doc = true)]
struct ReviewTemplate<'a> {
    project_name: &'a str,
    run: ResultRun,
    files: Vec<ResultFile>,
}

#[worker::send]
async fn index<S: GenericConfigProvider>(
    State(state): State<S>,
    Extension(auth_state): Extension<AuthState>,
) -> Result<Response, (StatusCode, &'static str)> {
    if !auth_state.permission_set.contains(UserPermissionSet::REVIEWER) {
        return Err((StatusCode::FORBIDDEN, "レビュワー権限が必要です。"));
    }
    let runs = results::list_runs(&state.bucket(), auth_state.project.id, None).await?;
    Ok(render(
        "完成稿確認",
        ReviewIndexTemplate {
            project_name: auth_state.project.name.as_ref(),
            runs: &runs,
        },
    ))
}

#[worker::send]
async fn review<S: GenericConfigProvider>(
    State(state): State<S>,
    Extension(auth_state): Extension<AuthState>,
    Path(RunPath { run_id }): Path<RunPath>,
) -> Result<Response, (StatusCode, &'static str)> {
    if !auth_state.permission_set.contains(UserPermissionSet::REVIEWER) {
        return Err((StatusCode::FORBIDDEN, "レビュワー権限が必要です。"));
    }
    let prefix = results::run_prefix(&auth_state, &run_id)?;
    let files = results::list_files(&state.bucket(), &prefix).await?;
    Ok(render(
        "完成稿確認",
        ReviewTemplate {
            project_name: auth_state.project.name.as_ref(),
            run: ResultRun::new(&run_id),
            files,
        },
    ))
}

#[worker::send]
async fn file<S: GenericConfigProvider>(
    State(state): State<S>,
    Extension(auth_state): Extension<AuthState>,
    Path(RunPath { run_id }): Path<RunPath>,
    Query(query): Query<FileQuery>,
) -> Result<Response, (StatusCode, &'static str)> {
    if !auth_state.permission_set.contains(UserPermissionSet::REVIEWER) {
        return Err((StatusCode::FORBIDDEN, "レビュワー権限が必要です。"));
    }
    let prefix = results::run_prefix(&auth_state, &run_id)?;
    results::serve_file(&state.bucket(), &prefix, query).await
}
