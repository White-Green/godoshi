use crate::app::template::HtmlTemplate;
use crate::auth::AuthState;
use crate::core::user::UserPermissionSet;
use askama::Template;
use axum::body::Body;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect};
use axum::{Extension, http};
use smol_str::format_smolstr;

/// ```askama
/// <h1 class="h2 fw-bold mb-4 text-break">{{ project_name }}</h1>
/// <p class="text-body-secondary mb-4">利用する機能を選択してください。</p>
/// <ul class="list-group shadow-sm">
/// {% if permissions.contains(UserPermissionSet::ADMIN) %}
///     <li class="list-group-item p-4"><a class="fw-semibold" href="/{{ project_name }}/admin">プロジェクト管理</a><p class="small text-body-secondary mb-0 mt-1">参加者の権限や提出項目を設定します。</p></li>
/// {% endif %}
/// {% if permissions.contains(UserPermissionSet::REVIEWER) %}
///     <li class="list-group-item p-4"><a class="fw-semibold" href="/{{ project_name }}/review">完成稿確認</a><p class="small text-body-secondary mb-0 mt-1">生成された完成稿の閲覧・ダウンロードができます。</p></li>
/// {% endif %}
/// {% if permissions.contains(UserPermissionSet::PARTICIPANT) %}
///     <li class="list-group-item p-4"><a class="fw-semibold" href="/{{ project_name }}/participant">原稿提出</a><p class="small text-body-secondary mb-0 mt-1">原稿ファイルや掲載情報を提出します。</p></li>
/// {% endif %}
/// </ul>
/// ```
#[derive(Debug, Template)]
#[template(ext = "html", in_doc = true)]
struct IndexTemplate<'a> {
    project_name: &'a str,
    permissions: UserPermissionSet,
}

pub async fn index(Extension(auth_state): Extension<AuthState>) -> http::Response<Body> {
    if auth_state.user.permissions().is_empty() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if auth_state.user.permissions().bits().count_ones() == 1 {
        if auth_state.user.is_admin() {
            return Redirect::to(&format_smolstr!("/{}/admin", auth_state.project.name)).into_response();
        } else if auth_state.user.is_participant() {
            return Redirect::to(&format_smolstr!("/{}/participant", auth_state.project.name)).into_response();
        } else if auth_state.user.is_reviewer() {
            return Redirect::to(&format_smolstr!("/{}/review", auth_state.project.name)).into_response();
        }
    }
    Html(
        HtmlTemplate::new(
            auth_state.project.name.as_ref(),
            IndexTemplate {
                project_name: auth_state.project.name.as_ref(),
                permissions: auth_state.user.permissions(),
            },
        )
        .render()
        .unwrap(),
    )
    .into_response()
}
