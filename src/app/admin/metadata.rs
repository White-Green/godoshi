use crate::app::notice::Notices;
use crate::app::{render, storage_error};
use crate::auth::AuthState;
use crate::core::Config;
use crate::core::metadata::{MAX_TEXT_BYTES, MetadataSchema};
use crate::core::project::ProjectStore;
use askama::Template;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Extension, Form};
use serde::Deserialize;

// HTTP request limit belongs to this screen, not the metadata storage model.
pub(super) const MAX_FORM_BYTES: usize = 32 * 1024 * 1024;

/// ```askama
/// <p><a href="/{{ project }}/admin">管理画面に戻る</a></p>
/// <h1>提出項目の設定</h1>
/// {% if saved %}<p role="status">保存しました。</p>{% endif %}
/// {% if !error.is_empty() %}<p role="alert">{{ error }}</p>{% endif %}
/// <form method="post" action="/{{ project }}/admin/metadata-schema">
/// <label>項目定義 JSON<br><textarea name="definition" rows="24" cols="90">{{ definition }}</textarea></label>
/// <p><button type="submit">保存</button></p>
/// </form>
/// <details><summary>設定例</summary><pre>{{ example }}</pre></details>
/// ```
#[derive(Template)]
#[template(ext = "html", in_doc = true)]
struct SchemaTemplate<'a> {
    project: &'a str,
    definition: &'a str,
    error: &'a str,
    saved: bool,
    example: &'a str,
}

const EXAMPLE: &str = r#"{
  "fields": [
    { "key": "display_name", "label": "表記名", "type": "string" },
    { "key": "title", "label": "タイトル", "type": "string" },
    { "key": "afterword", "label": "あとがき", "type": "string", "multiline": true },
    { "key": "icon", "label": "アイコン", "type": "file", "accept": "image/*" }
  ]
}"#;

fn schema_page(auth: &AuthState, definition: &str, error: &str, saved: bool) -> Response {
    render(
        "提出項目の設定",
        SchemaTemplate {
            project: auth.project.name.as_ref(),
            definition,
            error,
            saved,
            example: EXAMPLE,
        },
    )
}

pub(super) async fn schema<S: Config>(
    State(state): State<S>,
    Extension(auth): Extension<AuthState>,
    Extension(Notices(notices)): Extension<Notices>,
) -> Result<Response, (StatusCode, &'static str)> {
    if !auth.user.is_admin() {
        return Err((StatusCode::FORBIDDEN, "管理者権限が必要です。"));
    }
    let schema = state.projects().metadata_schema(auth.project.id).await.map_err(storage_error)?;
    let definition = serde_json::to_string_pretty(&schema).map_err(storage_error)?;
    Ok(schema_page(
        &auth,
        &definition,
        "",
        notices.get("saved").and_then(|v| v.as_bool()).unwrap_or_default(),
    ))
}

#[derive(Deserialize)]
pub(super) struct SchemaForm {
    definition: String,
}

pub(super) async fn save_schema<S: Config>(
    State(state): State<S>,
    Extension(auth): Extension<AuthState>,
    Form(form): Form<SchemaForm>,
) -> Result<Response, (StatusCode, &'static str)> {
    if !auth.user.is_admin() {
        return Err((StatusCode::FORBIDDEN, "管理者権限が必要です。"));
    }
    let parsed = if form.definition.len() > MAX_TEXT_BYTES {
        Err("項目定義は64 KiB以下にしてください。".to_owned())
    } else {
        serde_json::from_str::<MetadataSchema>(&form.definition)
            .map_err(|e| e.to_string())
            .and_then(|schema| schema.validate().map(|()| schema).map_err(|error| error.to_string()))
    };
    let schema = match parsed {
        Ok(schema) => schema,
        Err(error) => return Ok((StatusCode::UNPROCESSABLE_ENTITY, schema_page(&auth, &form.definition, &error, false)).into_response()),
    };
    state
        .projects()
        .save_metadata_schema(auth.project.id, schema)
        .await
        .map_err(storage_error)?;
    Ok((
        Extension(Notices::from([("saved", true)])),
        Redirect::to(&format!("/{}/admin/metadata-schema", auth.project.name)),
    )
        .into_response())
}
