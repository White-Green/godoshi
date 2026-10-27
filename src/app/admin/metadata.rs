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
/// <p class="mb-4"><a href="/{{ project }}/admin">管理画面に戻る</a></p>
/// <h1 class="h2 fw-bold mb-4">提出項目の設定</h1>
/// <p class="text-body-secondary">寄稿者に入力してもらう項目を設定します。ここでの並び順で掲載情報の入力画面に表示されます。</p>
/// {% if saved %}<p class="alert alert-success" role="status">保存しました。</p>{% endif %}
/// {% if !error.is_empty() %}<p class="alert alert-danger" role="alert">{{ error }}</p>{% endif %}
/// <form method="post" action="/{{ project }}/admin/metadata-schema" data-schema-form>
/// <div class="alert alert-danger" role="alert" data-schema-error hidden></div>
/// <div data-schema-editor hidden>
///     <div class="d-flex flex-wrap gap-2 justify-content-between align-items-center mb-3">
///         <h2 class="h5 mb-0">提出項目 <span class="badge text-bg-secondary" data-field-count></span></h2>
///         <span class="small text-body-secondary">最大64項目</span>
///     </div>
///     <div class="card mb-3" data-schema-empty><div class="card-body text-center py-5">
///         <h2 class="h5">提出項目はまだありません</h2><p class="text-body-secondary mb-0">「項目を追加」から、表記名やタイトルなどを追加してください。</p>
///     </div></div>
///     <div class="vstack gap-3" data-schema-fields></div>
///     <p class="form-text mt-3">既存の提出内容を引き継ぐ場合は、識別キーと入力形式を維持してください。項目の削除・変更は「設定を保存」で反映されます。</p>
/// </div>
/// <div data-schema-source>
///     <noscript><p class="alert alert-info">項目ごとの編集にはJavaScriptを有効にしてください。無効の場合はJSON形式で編集できます。</p></noscript>
///     <label class="form-label w-100">項目定義 JSON<textarea class="form-control font-monospace" name="definition" rows="16" data-schema-definition>{{ definition }}</textarea></label>
/// </div>
/// <div class="sticky-bottom bg-body-tertiary border-top d-flex flex-wrap gap-2 justify-content-between align-items-center mt-4 py-3">
///     <button class="btn btn-outline-primary" type="button" data-add-field hidden>＋ 項目を追加</button>
///     <button class="btn btn-primary px-4" type="submit">設定を保存</button>
/// </div>
/// <p class="visually-hidden" role="status" data-schema-status></p>
/// </form>
/// <template id="schema-field-template">
/// <section class="card schema-field">
/// <div class="card-body">
///     <div class="d-flex flex-wrap justify-content-between align-items-center gap-2 mb-3">
///         <h2 class="h6 mb-0 text-break" data-field-heading></h2>
///         <div class="col-12 col-sm-auto d-flex gap-2">
///             <button class="btn btn-sm btn-outline-secondary flex-fill" type="button" data-move-up>↑ 上へ</button>
///             <button class="btn btn-sm btn-outline-secondary flex-fill" type="button" data-move-down>↓ 下へ</button>
///             <button class="btn btn-sm btn-outline-danger flex-fill" type="button" data-remove-field>削除</button>
///         </div>
///     </div>
///     <div class="row g-3">
///         <div class="col-md-7"><label class="w-100"><span class="form-label d-block">項目名 <span class="text-danger small">必須</span></span>
///             <input class="form-control" type="text" data-field="label" required maxlength="1024" placeholder="例：表記名" />
///         </label></div>
///         <div class="col-md-5"><label class="w-100"><span class="form-label d-block">入力形式</span>
///             <select class="form-select" data-field="type"><option value="text">1行テキスト</option><option value="multiline">複数行テキスト</option><option value="file">ファイル</option></select>
///         </label></div>
///         <div class="col-12"><label class="w-100"><span class="form-label d-block">説明 <span class="text-body-secondary fw-normal small">任意</span></span>
///             <textarea class="form-control" data-field="description" rows="2" maxlength="4096" placeholder="寄稿者に伝えたい入力の案内"></textarea>
///         </label></div>
///         <div class="col-md-7"><label class="w-100"><span class="form-label d-block">識別キー <span class="text-danger small">必須</span></span>
///             <input class="form-control font-monospace" type="text" data-field="key" required maxlength="64" pattern="[a-z][a-z0-9_]{0,63}" spellcheck="false" autocapitalize="none" />
///         </label><p class="form-text mb-0" data-key-help>半角英小文字で始め、英小文字・数字・_ を使います。他の項目と重複できません。</p></div>
///         <div class="col-md-5" data-text-options><label class="w-100"><span class="form-label d-block">最大文字数 <span class="text-body-secondary fw-normal small">任意</span></span>
///             <input class="form-control" type="number" data-field="max_length" min="1" max="65536" step="1" placeholder="指定なし" />
///         </label></div>
///         <div class="col-md-5" data-file-options hidden><label class="w-100"><span class="form-label d-block">選択できるファイル <span class="text-body-secondary fw-normal small">任意</span></span>
///             <input class="form-control" type="text" data-field="accept" maxlength="1024" placeholder="例：image/* または .pdf,.zip" />
///         </label><p class="form-text mb-0" data-accept-help>空欄ならすべての種類。1ファイル5 MiBまで。</p></div>
///     </div>
/// </div>
/// </section>
/// </template>
/// <script src="/static/schema-editor.js" defer></script>
/// ```
#[derive(Template)]
#[template(ext = "html", in_doc = true)]
struct SchemaTemplate<'a> {
    project: &'a str,
    definition: &'a str,
    error: &'a str,
    saved: bool,
}

fn schema_page(auth: &AuthState, definition: &str, error: &str, saved: bool) -> Response {
    render(
        "提出項目の設定",
        SchemaTemplate {
            project: auth.project.name.as_ref(),
            definition,
            error,
            saved,
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
