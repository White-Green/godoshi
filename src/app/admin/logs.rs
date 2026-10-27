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

/// ```askama
/// <p><a href="/{{ project_name }}/admin">管理画面に戻る</a></p>
/// <h1>操作ログ</h1>
/// <p>新しい順に50件ずつ表示します。</p>
/// <p><a href="/{{ project_name }}/admin/logs">最新のログに戻る</a></p>
/// <ul>
/// {% for entry in page.entries %}
///     <li>{{ entry.datetime() }} — {{ entry.event }}</li>
/// {% else %}
///     <li>このページに操作ログはありません。</li>
/// {% endfor %}
/// </ul>
/// {% if let Some(cursor) = page.next_cursor %}
/// <p><a rel="next" href="/{{ project_name }}/admin/logs?cursor={{ cursor|urlencode_strict }}">次の50件（古いログ）</a></p>
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
        "操作ログ",
        LogsTemplate {
            project_name: auth.project.name.as_ref(),
            page,
        },
    ))
}
