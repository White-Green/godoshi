use super::notice::Notices;
use super::{render, storage_error};
use crate::auth::AuthState;
use crate::core::Config;
use crate::core::user::discord::{DiscordInitMemberError, DiscordUserStore};
use crate::core::user::{ProjectMember, ProjectUserStore, UserId, UserPermissionSet, UserStore, Username};
use askama::Template;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Extension, Form, Router};
use serde::Deserialize;
use smol_str::format_smolstr;

pub fn route<S>() -> Router<S>
where
    S: Config + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/admin", axum::routing::get(admin::<S>))
        .route("/admin/update_permission", axum::routing::post(update_permission::<S>))
        .route("/admin/add_user", axum::routing::post(add_user::<S>))
}

/// ```askama
/// {% if show_project_link %}<p><a href="/{{ project_name }}/">戻る</a></p>{% endif %}
/// <h1>参加者管理</h1>
/// {% if saved %}<p role="status">保存しました。</p>{% endif %}
/// {% for user in users %}
/// <form action="/{{ project_name }}/admin/update_permission" method="post">
///     <div>{{ user.username() }}</div>
///     <input type="hidden" name="user_id" value="{{ user.user_id() }}" />
///     <label><input type="checkbox" name="admin" value="true" {% if user.is_admin() %}checked{% endif %} />プロジェクト管理者</label>
///     <label><input type="checkbox" name="reviewer" value="true" {% if user.is_reviewer() %}checked{% endif %} />レビュワー</label>
///     <label><input type="checkbox" name="participant" value="true" {% if user.is_participant() %}checked{% endif %} />寄稿者</label>
///     <input type="submit" value="更新" />
/// </form>
/// {% else %}<p>参加者はまだいません。</p>{% endfor %}
/// <h2>ユーザー追加</h2>
/// <form action="/{{ project_name }}/admin/add_user" method="post">
///     <label>Discord ユーザー名 <input type="text" name="username" required maxlength="32" /></label>
///     <label><input type="checkbox" name="admin" value="true" />プロジェクト管理者</label>
///     <label><input type="checkbox" name="reviewer" value="true" />レビュワー</label>
///     <label><input type="checkbox" name="participant" value="true" checked />寄稿者</label>
///     <input type="submit" value="追加" />
/// </form>
/// ```
#[derive(Debug, Template)]
#[template(ext = "html", in_doc = true)]
struct AdminTemplate<'a> {
    project_name: &'a str,
    show_project_link: bool,
    users: Vec<ProjectMember>,
    saved: bool,
}

#[derive(Deserialize)]
struct Permissions {
    admin: Option<Checkbox>,
    reviewer: Option<Checkbox>,
    participant: Option<Checkbox>,
}

#[derive(Deserialize)]
enum Checkbox {
    #[serde(rename = "true", alias = "on")]
    Checked,
}

impl Permissions {
    fn permission_set(&self) -> UserPermissionSet {
        let mut permissions = UserPermissionSet::empty();
        permissions.set(UserPermissionSet::ADMIN, self.admin.is_some());
        permissions.set(UserPermissionSet::REVIEWER, self.reviewer.is_some());
        permissions.set(UserPermissionSet::PARTICIPANT, self.participant.is_some());
        permissions
    }
}

#[derive(Deserialize)]
struct UpdatePermission {
    user_id: UserId,
    #[serde(flatten)]
    permissions: Permissions,
}

#[derive(Deserialize)]
struct AddUser {
    username: String,
    #[serde(flatten)]
    permissions: Permissions,
}

async fn admin<S: Config>(
    State(state): State<S>,
    Extension(auth_state): Extension<AuthState>,
    Extension(Notices(notices)): Extension<Notices>,
) -> Result<Response, (StatusCode, &'static str)> {
    if !auth_state.user.is_admin() {
        return Err((StatusCode::FORBIDDEN, "プロジェクト管理者権限が必要です。"));
    }
    let users = state.users().project(auth_state.project.id).all_member().await.map_err(storage_error)?;
    Ok(render(
        "参加者管理",
        AdminTemplate {
            project_name: auth_state.project.name.as_ref(),
            show_project_link: auth_state.user.permissions().bits().count_ones() >= 2,
            users,
            saved: notices.get("saved").and_then(|value| value.as_bool()).unwrap_or_default(),
        },
    ))
}

async fn update_permission<S: Config>(
    State(state): State<S>,
    Extension(auth_state): Extension<AuthState>,
    Form(form): Form<UpdatePermission>,
) -> Result<Response, (StatusCode, &'static str)> {
    if !auth_state.user.is_admin() {
        return Err((StatusCode::FORBIDDEN, "プロジェクト管理者権限が必要です。"));
    }
    let permissions = if form.user_id == auth_state.user.user_id() {
        form.permissions.permission_set() | UserPermissionSet::ADMIN
    } else {
        form.permissions.permission_set()
    };
    let updated = state
        .users()
        .project(auth_state.project.id)
        .update_permission(form.user_id, permissions)
        .await
        .map_err(storage_error)?;
    if !updated {
        return Err((StatusCode::CONFLICT, "対象の参加者がいません。"));
    }
    Ok((
        Extension(Notices::from([("saved", true)])),
        Redirect::to(&format_smolstr!("/{}/admin", auth_state.project.name)),
    )
        .into_response())
}

async fn add_user<S: Config>(
    State(state): State<S>,
    Extension(auth_state): Extension<AuthState>,
    Form(form): Form<AddUser>,
) -> Result<Response, (StatusCode, &'static str)> {
    if !auth_state.user.is_admin() {
        return Err((StatusCode::FORBIDDEN, "プロジェクト管理者権限が必要です。"));
    }
    let username = Username::new(form.username.trim().into()).ok_or((StatusCode::BAD_REQUEST, "Discord ユーザー名を入力してください。"))?;
    state
        .users()
        .project(auth_state.project.id)
        .init_member(username, form.permissions.permission_set())
        .await
        .map_err(|error| match error {
            DiscordInitMemberError::DuplicatedUsername => (StatusCode::CONFLICT, "同名のユーザーが複数存在するため追加できません。"),
            DiscordInitMemberError::AlreadyMember => (StatusCode::CONFLICT, "すでに参加しているユーザーです。権限は一覧から更新してください。"),
            DiscordInitMemberError::Other(error) => storage_error(error),
        })?;
    Ok((
        Extension(Notices::from([("saved", true)])),
        Redirect::to(&format_smolstr!("/{}/admin", auth_state.project.name)),
    )
        .into_response())
}
