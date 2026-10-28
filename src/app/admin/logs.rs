use crate::app::{bucket_error, render};
use crate::auth::AuthState;
use crate::core::Config;
use crate::core::bucket::{Bucket, ProjectBucket};
use crate::core::operation_log::OperationLogPage;
use askama::Template;
use axum::Extension;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use serde::Deserialize;

#[derive(Deserialize)]
pub(super) struct LogQuery {
    cursor: Option<String>,
}

pub(super) const TITLE: &str = "操作ログ";

/// ```askama
/// <div class="d-flex flex-wrap justify-content-between align-items-start gap-3 mb-4">
///     <div><h1 class="h2 fw-bold mb-2">{{ TITLE }}</h1><p class="text-body-secondary mb-0">新しい順に50件ずつ表示します。</p></div>
///     <a class="btn btn-outline-primary" href="/{{ project_name }}/admin/logs">最新のログに戻る</a>
/// </div>
/// <div class="card">
/// <ul class="list-group list-group-flush">
/// {% for entry in page.entries %}
///     <li class="list-group-item px-3 py-1 d-flex flex-column flex-md-row align-items-md-baseline gap-md-3">
///         <time class="small text-body-secondary flex-shrink-0 text-nowrap">{{ entry.datetime() }}</time>
///         <div class="text-break">{{ entry.event }}</div>
///     </li>
/// {% else %}
///     <li class="list-group-item p-4 text-body-secondary">このページに操作ログはありません。</li>
/// {% endfor %}
/// </ul>
/// </div>
/// {% if let Some(cursor) = page.next_cursor %}
/// <nav class="d-flex justify-content-end mt-4" aria-label="操作ログのページ送り">
///     <a class="btn btn-outline-primary" rel="next" href="/{{ project_name }}/admin/logs?cursor={{ cursor|urlencode_strict }}">次の50件（古いログ）</a>
/// </nav>
/// {% endif %}
/// ```
#[derive(Template)]
#[template(ext = "html", in_doc = true)]
struct LogsTemplate<'a> {
    project_name: &'a str,
    page: OperationLogPage,
}

pub(super) async fn index<S: Config>(
    State(state): State<S>,
    Extension(auth): Extension<AuthState>,
    Query(query): Query<LogQuery>,
) -> Result<Response, (StatusCode, &'static str)> {
    if !auth.user.is_admin() {
        return Err((StatusCode::FORBIDDEN, "プロジェクト管理者権限が必要です。"));
    }
    let page = state
        .bucket()
        .project(auth.project.id)
        .logs(50, query.cursor.as_deref())
        .await
        .map_err(bucket_error)?;
    Ok(render(
        &auth,
        &[(crate::app::root::TITLE, ""), (super::TITLE, "admin"), (TITLE, "admin/logs")],
        LogsTemplate {
            project_name: auth.project.name.as_ref(),
            page,
        },
    ))
}
