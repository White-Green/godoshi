use super::{render, storage_error};
use crate::auth::AuthState;
use crate::config::GenericConfigProvider;
use crate::user::{UserId, UserPermissionSet};
use askama::Template;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Extension, Form, Router};
use serde::Deserialize;
use smol_str::format_smolstr;

pub fn route<S>() -> Router<S>
where
    S: GenericConfigProvider + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/admin", axum::routing::get(admin::<S>))
        .route("/admin/update_permission", axum::routing::post(update_permission::<S>))
        .route("/admin/add_user", axum::routing::post(add_user::<S>))
}

/// ```askama
/// <p><a href="/{{ project_name }}/">戻る</a></p>
/// <h1>参加者管理</h1>
/// {% if saved %}<p role="status">保存しました。</p>{% endif %}
/// {% for user in users %}
/// <form action="/{{ project_name }}/admin/update_permission" method="post">
///     <div>{{ user.name }} ({{ user.id }})</div>
///     <input type="hidden" name="user_id" value="{{ user.id }}" />
///     <label><input type="checkbox" name="admin" value="true" {% if user.permission_set.contains(UserPermissionSet::ADMIN) %}checked{% endif %} />プロジェクト管理者</label>
///     <label><input type="checkbox" name="reviewer" value="true" {% if user.permission_set.contains(UserPermissionSet::REVIEWER) %}checked{% endif %} />レビュワー</label>
///     <label><input type="checkbox" name="participant" value="true" {% if user.permission_set.contains(UserPermissionSet::PARTICIPANT) %}checked{% endif %} />寄稿者</label>
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
    users: Vec<AdminUser>,
    saved: bool,
}

#[derive(Debug, Deserialize)]
struct AdminUser {
    id: u64,
    name: String,
    permission_set: UserPermissionSet,
}

#[derive(Default, Deserialize)]
struct Notice {
    #[serde(default)]
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

#[worker::send]
async fn admin<S: GenericConfigProvider>(
    State(state): State<S>,
    Extension(auth_state): Extension<AuthState>,
    Query(notice): Query<Notice>,
) -> Result<Response, (StatusCode, &'static str)> {
    if !auth_state.permission_set.contains(UserPermissionSet::ADMIN) {
        return Err((StatusCode::FORBIDDEN, "プロジェクト管理者権限が必要です。"));
    }
    let db = state.db();
    let users = worker::query!(
        &db,
        "select p.user_id as id, p.username as name, p.permission_set
         from project_member p
         where p.project_id = ? order by name, p.user_id",
        auth_state.project.id
    )
    .expect("Admin query parameters must be valid D1 values")
    .all()
    .await
    .map_err(storage_error)?
    .results::<AdminUser>()
    .expect("Admin query results must match the schema");
    return Ok(render(
        "参加者管理",
        AdminTemplate {
            project_name: auth_state.project.name.as_ref(),
            users,
            saved: notice.saved,
        },
    ));
}

#[worker::send]
async fn update_permission<S: GenericConfigProvider>(
    State(state): State<S>,
    Extension(auth_state): Extension<AuthState>,
    Form(form): Form<UpdatePermission>,
) -> Result<Response, (StatusCode, &'static str)> {
    if !auth_state.permission_set.contains(UserPermissionSet::ADMIN) {
        return Err((StatusCode::FORBIDDEN, "プロジェクト管理者権限が必要です。"));
    }
    let permissions = if form.user_id == auth_state.user_id {
        form.permissions.permission_set() | UserPermissionSet::ADMIN
    } else {
        form.permissions.permission_set()
    };
    let db = state.db();
    // Keep at least one administrator, including when requests arrive concurrently.
    let updated = worker::query!(
        &db,
        "update project_member set permission_set = ? where project_id = ? and user_id = ?
         returning user_id",
        permissions,
        auth_state.project.id,
        form.user_id
    )
    .expect("Permission update parameters must be valid D1 values")
    .first::<UserId>(Some("user_id"))
    .await
    .map_err(storage_error)?;
    if updated.is_none() {
        return Err((StatusCode::CONFLICT, "対象の参加者がいません。"));
    }
    Ok(Redirect::to(&format_smolstr!("/{}/admin?saved=true", auth_state.project.name)).into_response())
}

#[worker::send]
async fn add_user<S: GenericConfigProvider>(
    State(state): State<S>,
    Extension(auth_state): Extension<AuthState>,
    Form(form): Form<AddUser>,
) -> Result<Response, (StatusCode, &'static str)> {
    if !auth_state.permission_set.contains(UserPermissionSet::ADMIN) {
        return Err((StatusCode::FORBIDDEN, "プロジェクト管理者権限が必要です。"));
    }
    let username = form.username.trim();
    if username.is_empty() || username.len() > 32 || username.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err((StatusCode::BAD_REQUEST, "Discord ユーザー名を入力してください。"));
    }
    let permissions = form.permissions.permission_set();
    let db = state.db();
    let users = worker::query!(&db, "select id from discord_user where username = ?", username)
        .expect("Username query parameters must be valid D1 values")
        .all()
        .await
        .map_err(storage_error)?
        .results::<ExistingUser>()
        .expect("User query results must match the schema");
    if users.len() > 1 {
        return Err((StatusCode::CONFLICT, "同名のユーザーが複数存在するため追加できません。"));
    }
    // D1 batches are atomic. Reuse an existing Discord account without changing its identity or overwriting permissions when the add form is submitted twice.
    // ref: https://developers.cloudflare.com/d1/worker-api/d1-database/#batch
    let results = db
        .batch(vec![
            worker::query!(
                &db,
                "insert into user (id) select null where not exists (select 1 from discord_user where username = ?)",
                username
            )
            .expect("User insert parameters must be valid D1 values"),
            worker::query!(
                &db,
                "insert into discord_user (id, username) select last_insert_rowid(), ? where changes() = 1",
                username
            )
            .expect("Discord user insert parameters must be valid D1 values"),
            worker::query!(
                &db,
                "insert into project_member (project_id, user_id, username, permission_set)
                 select ?, id, username, ? from discord_user where username = ?
                 on conflict (project_id, user_id) do nothing returning user_id",
                auth_state.project.id,
                permissions,
                username
            )
            .expect("Project member insert parameters must be valid D1 values"),
        ])
        .await
        .map_err(storage_error)?;
    if results[2]
        .results::<AddedUser>()
        .expect("Project member insert results must match the schema")
        .is_empty()
    {
        return Err((StatusCode::CONFLICT, "すでに参加しているユーザーです。権限は一覧から更新してください。"));
    }
    Ok(Redirect::to(&format_smolstr!("/{}/admin?saved=true", auth_state.project.name)).into_response())
}

#[derive(Deserialize)]
struct ExistingUser {
    #[serde(rename = "id")]
    _id: UserId,
}

#[derive(Deserialize)]
struct AddedUser {
    #[serde(rename = "user_id")]
    _user_id: UserId,
}
