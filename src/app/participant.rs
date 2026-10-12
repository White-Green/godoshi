use super::results::{self, FileQuery, ResultFile, ResultRun, RunPath};
use super::{render, storage_error};
use crate::auth::AuthState;
use crate::config::GenericConfigProvider;
use crate::user::UserPermissionSet;
use askama::Template;
use axum::body::{Body, to_bytes};
use axum::extract::{Path, Query, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Extension, Router};
use serde::Deserialize;
use smol_str::format_smolstr;
use worker::{FormEntry, HttpMetadata};

const MAX_FILE_SIZE: usize = 20 * 1024 * 1024;
const MAX_FORM_SIZE: usize = MAX_FILE_SIZE + 64 * 1024;

pub fn route<S>() -> Router<S>
where
    S: GenericConfigProvider + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/participant", axum::routing::get(participant::<S>))
        .route("/participant/upload", axum::routing::post(upload::<S>))
        .route("/participant/{run_id}", axum::routing::get(completed::<S>))
        .route("/participant/{run_id}/file", axum::routing::get(file::<S>))
}

/// ```askama
/// <p><a href="/{{ project_name }}/">戻る</a></p>
/// <h1>原稿提出</h1>
/// {% if uploaded %}<p role="status">アップロードしました。</p>{% endif %}
/// <p>1ファイル20 MiBまで。同じ名前のファイルは上書きされます。</p>
/// <form action="/{{ project_name }}/participant/upload" method="post" enctype="multipart/form-data">
///     <label>原稿ファイル <input type="file" name="file" required /></label>
///     <input type="submit" value="アップロード" />
/// </form>
/// <h2>完成稿確認</h2>
/// <ul>
/// {% for run in runs %}
///     <li><a href="/{{ project_name }}/participant/{{ run.key }}">{{ run.name }}</a></li>
/// {% else %}<li>完成稿はまだありません。</li>{% endfor %}
/// </ul>
/// ```
#[derive(Debug, Template)]
#[template(ext = "html", in_doc = true)]
struct ParticipantTemplate<'a> {
    project_name: &'a str,
    uploaded: bool,
    runs: Vec<ResultRun>,
}

/// ```askama
/// <p><a href="/{{ project_name }}/participant">一覧に戻る</a></p>
/// <h1>完成稿確認</h1>
/// <h2>{{ run.name }}</h2>
/// <ul>
/// {% for file in files %}
///     <li>{{ file.name }} ({{ file.size }} bytes)
///         <a href="/{{ project_name }}/participant/{{ run.key }}/file?key={{ file.key }}" target="_blank" rel="noopener">閲覧</a>
///         <a href="/{{ project_name }}/participant/{{ run.key }}/file?key={{ file.key }}&amp;download=true">ダウンロード</a>
///     </li>
/// {% else %}<li>完成稿はまだありません。</li>{% endfor %}
/// </ul>
/// ```
#[derive(Debug, Template)]
#[template(ext = "html", in_doc = true)]
struct CompletedTemplate<'a> {
    project_name: &'a str,
    run: ResultRun,
    files: Vec<ResultFile>,
}

#[derive(Deserialize)]
struct Notice {
    #[serde(default)]
    uploaded: bool,
}

#[worker::send]
async fn participant<S: GenericConfigProvider>(
    State(state): State<S>,
    Extension(auth_state): Extension<AuthState>,
    Query(notice): Query<Notice>,
) -> Result<Response, (StatusCode, &'static str)> {
    if !auth_state.permission_set.contains(UserPermissionSet::PARTICIPANT) {
        return Err((StatusCode::FORBIDDEN, "寄稿者権限が必要です。"));
    }
    let username = author_username(&auth_state);
    let runs = results::list_runs(&state.bucket(), auth_state.project.id, Some(username)).await?;
    Ok(render(
        "原稿提出",
        ParticipantTemplate {
            project_name: auth_state.project.name.as_ref(),
            uploaded: notice.uploaded,
            runs,
        },
    ))
}

#[worker::send]
async fn completed<S: GenericConfigProvider>(
    State(state): State<S>,
    Extension(auth_state): Extension<AuthState>,
    Path(RunPath { run_id }): Path<RunPath>,
) -> Result<Response, (StatusCode, &'static str)> {
    if !auth_state.permission_set.contains(UserPermissionSet::PARTICIPANT) {
        return Err((StatusCode::FORBIDDEN, "寄稿者権限が必要です。"));
    }
    let prefix = author_prefix(&auth_state, &run_id)?;
    let files = results::list_files(&state.bucket(), &prefix).await?;
    Ok(render(
        "完成稿確認",
        CompletedTemplate {
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
    if !auth_state.permission_set.contains(UserPermissionSet::PARTICIPANT) {
        return Err((StatusCode::FORBIDDEN, "寄稿者権限が必要です。"));
    }
    let prefix = author_prefix(&auth_state, &run_id)?;
    results::serve_file(&state.bucket(), &prefix, query).await
}

fn author_username(auth_state: &AuthState) -> &str {
    assert!(results::valid_key(&auth_state.username) && !auth_state.username.contains('/'));
    &auth_state.username
}

fn author_prefix(auth_state: &AuthState, run_id: &str) -> Result<String, (StatusCode, &'static str)> {
    let prefix = results::run_prefix(auth_state, run_id)?;
    let username = author_username(auth_state);
    let prefix = format!("{prefix}authors/{username}/");
    if prefix.len() > 1024 {
        return Err((StatusCode::BAD_REQUEST, "実行IDが不正です。"));
    }
    Ok(prefix)
}

#[worker::send]
async fn upload<S: GenericConfigProvider>(
    State(state): State<S>,
    Extension(auth_state): Extension<AuthState>,
    request: Request,
) -> Result<Response, (StatusCode, &'static str)> {
    if !auth_state.permission_set.contains(UserPermissionSet::PARTICIPANT) {
        return Err((StatusCode::FORBIDDEN, "寄稿者権限が必要です。"));
    }
    // Bound memory before letting the runtime parse the multipart form.
    let (parts, body) = request.into_parts();
    let body = to_bytes(body, MAX_FORM_SIZE)
        .await
        .map_err(|_| (StatusCode::PAYLOAD_TOO_LARGE, "ファイルは20 MiB以下にしてください。"))?;
    let mut request: worker::Request = Request::from_parts(parts, Body::from(body)).try_into().map_err(storage_error)?;
    let form = request
        .form_data()
        .await
        .map_err(|_| (StatusCode::BAD_REQUEST, "ファイルを選択してください。"))?;
    let mut files = form.get_all("file").unwrap_or_default();
    if files.len() != 1 {
        return Err((StatusCode::BAD_REQUEST, "ファイルを1つ選択してください。"));
    }
    let Some(FormEntry::File(file)) = files.pop() else {
        return Err((StatusCode::BAD_REQUEST, "ファイルを選択してください。"));
    };
    let filename = file.name();
    let key = format!("projects/{}/resources/authors/{filename}", auth_state.project.id);
    if filename.trim().is_empty()
        || matches!(filename.as_str(), "." | "..")
        || filename.contains(['/', '\\'])
        || filename.chars().any(char::is_control)
        || key.len() > 1024
    {
        return Err((StatusCode::BAD_REQUEST, "ファイル名が不正です。"));
    }
    if file.size() > MAX_FILE_SIZE {
        return Err((StatusCode::PAYLOAD_TOO_LARGE, "ファイルは20 MiB以下にしてください。"));
    }
    let bytes = file.bytes().await.map_err(storage_error)?;
    state
        .bucket()
        .put(key, bytes)
        .http_metadata(HttpMetadata {
            content_type: Some("application/octet-stream".into()),
            ..Default::default()
        })
        .execute()
        .await
        .map_err(storage_error)?;
    Ok(Redirect::to(&format_smolstr!("/{}/participant?uploaded=true", auth_state.project.name)).into_response())
}
