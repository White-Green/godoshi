use super::notice::Notices;
use super::{render, storage_error};
use crate::auth::AuthState;
use crate::core::Config;
use crate::core::project::ProjectStore;
use crate::core::user::discord::{DiscordInitMemberError, DiscordUserStore};
use crate::core::user::{ProjectMember, ProjectUserStore, UserId, UserPermissionSet, UserStore, Username};
use askama::Template;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Extension, Form, Router};
use base64::Engine;
use serde::Deserialize;
use smol_str::format_smolstr;

mod metadata;

pub fn route<S>() -> Router<S>
where
    S: Config + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/admin", axum::routing::get(admin::<S>))
        .route(
            "/admin/metadata-schema",
            axum::routing::get(metadata::schema::<S>)
                .post(metadata::save_schema::<S>)
                .layer(DefaultBodyLimit::max(metadata::MAX_FORM_BYTES)),
        )
        .route("/admin/update_permission", axum::routing::post(update_permission::<S>))
        .route("/admin/add_user", axum::routing::post(add_user::<S>))
        .route("/admin/environment_upload/create", axum::routing::post(create_environment_upload::<S>))
        .route("/admin/environment_upload/delete", axum::routing::post(delete_environment_upload::<S>))
}

/// ```askama
/// {% if show_project_link %}<p><a href="/{{ project_name }}/">戻る</a></p>{% endif %}
/// <h1>プロジェクト管理</h1>
/// <p><a href="/{{ project_name }}/admin/metadata-schema">提出項目の設定</a></p>
/// {% if saved %}<p role="status">保存しました。</p>{% endif %}
/// <h2>ビルド環境のアップロード</h2>
/// {% if let Some(url) = environment_upload_url %}
/// <label>アップロード用URL取得先 <input type="text" value="{{ url }}" readonly size="100" /></label>
/// <p>このURLへGETし、返されたJSONのurlへファイルをPUTしてください。</p>
/// <form action="/{{ project_name }}/admin/environment_upload/delete" method="post">
///     <input type="hidden" name="upload_key" value="{{ environment_upload_key.as_deref().unwrap_or_default() }}" />
///     <input type="submit" value="アップロードURLを削除（無効化）" />
/// </form>
/// {% else %}
/// <form action="/{{ project_name }}/admin/environment_upload/create" method="post">
///     <input type="submit" value="アップロードURLを発行" />
/// </form>
/// {% endif %}
/// <h2>参加者管理</h2>
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
/// <h3>ユーザー追加</h3>
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
    environment_upload_key: Option<String>,
    environment_upload_url: Option<String>,
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

#[derive(Deserialize)]
struct DeleteEnvironmentUpload {
    upload_key: String,
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
    let environment_upload_key = state
        .projects()
        .environment_upload_key(auth_state.project.id)
        .await
        .map_err(storage_error)?;
    let environment_upload_url = environment_upload_key
        .as_ref()
        .map(|key| format!("{}/upload/{key}", state.base_url().trim_end_matches('/')));
    Ok(render(
        "プロジェクト管理",
        AdminTemplate {
            project_name: auth_state.project.name.as_ref(),
            show_project_link: auth_state.user.permissions().bits().count_ones() >= 2,
            users,
            saved: notices.get("saved").and_then(|value| value.as_bool()).unwrap_or_default(),
            environment_upload_key,
            environment_upload_url,
        },
    ))
}

async fn create_environment_upload<S: Config>(
    State(state): State<S>,
    Extension(auth_state): Extension<AuthState>,
) -> Result<Response, (StatusCode, &'static str)> {
    if !auth_state.user.is_admin() {
        return Err((StatusCode::FORBIDDEN, "プロジェクト管理者権限が必要です。"));
    }
    let mut random = [0; 32];
    getrandom::fill(&mut random).map_err(storage_error)?;
    let key = format!(
        "{}-{}",
        auth_state.project.name,
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random)
    );
    state
        .projects()
        .create_environment_upload_key(auth_state.project.id, &key)
        .await
        .map_err(storage_error)?;
    Ok((
        Extension(Notices::from([("saved", true)])),
        Redirect::to(&format_smolstr!("/{}/admin", auth_state.project.name)),
    )
        .into_response())
}

async fn delete_environment_upload<S: Config>(
    State(state): State<S>,
    Extension(auth_state): Extension<AuthState>,
    Form(form): Form<DeleteEnvironmentUpload>,
) -> Result<Response, (StatusCode, &'static str)> {
    if !auth_state.user.is_admin() {
        return Err((StatusCode::FORBIDDEN, "プロジェクト管理者権限が必要です。"));
    }
    state
        .projects()
        .delete_environment_upload_key(auth_state.project.id, &form.upload_key)
        .await
        .map_err(storage_error)?;
    Ok((
        Extension(Notices::from([("saved", true)])),
        Redirect::to(&format_smolstr!("/{}/admin", auth_state.project.name)),
    )
        .into_response())
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
