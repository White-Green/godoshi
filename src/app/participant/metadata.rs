use crate::app::notice::Notices;
use crate::app::{bucket_error, render, storage_error};
use crate::auth::AuthState;
use crate::core::Config;
use crate::core::bucket::{Bucket, ProjectBucket};
use crate::core::build::BuildRequests;
use crate::core::http::FileResponse;
use crate::core::metadata::*;
use crate::core::project::ProjectStore;
use askama::Template;
use axum::Extension;
use axum::extract::multipart::{Field, MultipartError, MultipartRejection};
use axum::extract::{Multipart, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use bytes::Bytes;
use std::collections::BTreeSet;

// Multipart envelope limit is independent of the per-value storage limits.
pub(super) const MAX_FORM_BYTES: usize = 32 * 1024 * 1024;

struct FieldView {
    key: String,
    label: String,
    description: String,
    input: FieldInput,
}

enum FieldInput {
    String { value: String, multiline: bool, max_length: usize },
    File { filename: String, accept: String },
}

/// ```askama
/// <p class="mb-4"><a href="/{{ project }}/participant">原稿提出に戻る</a></p>
/// <h1 class="h2 fw-bold mb-4">掲載情報の提出</h1>
/// {% if saved %}<p class="alert alert-success" role="status">保存しました。</p>{% endif %}
/// {% if !error.is_empty() %}<p class="alert alert-danger" role="alert">{{ error }} ファイルを変更する場合は選び直してください。</p>{% endif %}
/// {% if fields.is_empty() %}<p class="alert alert-light border">提出項目はまだ設定されていません。</p>{% else %}
/// <form method="post" action="/{{ project }}/participant/metadata" enctype="multipart/form-data">
/// {% for field in fields %}
///     <fieldset class="card mb-3 p-3 p-md-4"><legend class="float-none w-auto fs-6 px-2">{{ field.label }}</legend>
///     {% if !field.description.is_empty() %}<p class="text-body-secondary" id="description-{{ field.key }}">{{ field.description }}</p>{% endif %}
///     {% match field.input %}
///     {% when FieldInput::File { filename, accept } %}
///         {% if !filename.is_empty() %}
///             <p class="text-break"><a href="/{{ project }}/participant/metadata/assets/{{ field.key }}">{{ filename }}</a></p>
///             <label class="mb-3"><input class="form-check-input me-2" type="checkbox" name="remove.{{ field.key }}" value="true">削除する</label>
///         {% endif %}
///         <label class="form-label">ファイルを選択 <input class="form-control mt-2" type="file" name="file.{{ field.key }}" accept="{{ accept }}" {% if !field.description.is_empty() %}aria-describedby="description-{{ field.key }}"{% endif %}></label>
///         <p class="form-text mb-0">1ファイル5 MiBまで。</p>
///     {% when FieldInput::String { value, multiline, max_length } %}
///         {% if multiline %}
///             <textarea class="form-control" name="string.{{ field.key }}" aria-label="{{ field.label }}" {% if !field.description.is_empty() %}aria-describedby="description-{{ field.key }}"{% endif %} rows="6" maxlength="{{ max_length }}">{{ value }}</textarea>
///         {% else %}
///             <input class="form-control" type="text" name="string.{{ field.key }}" aria-label="{{ field.label }}" {% if !field.description.is_empty() %}aria-describedby="description-{{ field.key }}"{% endif %} value="{{ value }}" maxlength="{{ max_length }}">
///         {% endif %}
///     {% endmatch %}
///     </fieldset>
/// {% endfor %}
/// <p class="mt-4"><button class="btn btn-primary px-4" type="submit">保存</button></p>
/// </form>{% endif %}
/// ```
#[derive(Template)]
#[template(ext = "html", in_doc = true)]
struct EditTemplate<'a> {
    project: &'a str,
    fields: Vec<FieldView>,
    saved: bool,
    error: &'a str,
}

fn edit_page(auth: &AuthState, schema: MetadataSchema, metadata: &AuthorMetadata, saved: bool, error: &str) -> Response {
    let fields = schema
        .fields
        .into_iter()
        .map(|field| FieldView {
            input: match field.kind {
                FieldKind::String { multiline, max_length } => FieldInput::String {
                    value: metadata.values.get(&field.key).cloned().unwrap_or_default(),
                    multiline,
                    max_length: max_length.unwrap_or(MAX_TEXT_BYTES),
                },
                FieldKind::File { accept } => FieldInput::File {
                    filename: metadata.files.get(&field.key).cloned().unwrap_or_default(),
                    accept: accept.unwrap_or_default(),
                },
            },
            key: field.key,
            label: field.label,
            description: field.description,
        })
        .collect();
    render(
        "掲載情報の提出",
        EditTemplate {
            project: auth.project.name.as_ref(),
            fields,
            saved,
            error,
        },
    )
}

pub(super) async fn edit<S: Config>(
    State(state): State<S>,
    Extension(auth): Extension<AuthState>,
    Extension(Notices(notices)): Extension<Notices>,
) -> Result<Response, (StatusCode, &'static str)> {
    if !auth.user.is_participant() {
        return Err((StatusCode::FORBIDDEN, "寄稿者権限が必要です。"));
    }
    let schema = state.projects().metadata_schema(auth.project.id).await.map_err(storage_error)?;
    let metadata = state
        .bucket()
        .project(auth.project.id)
        .metadata_by_user(auth.user.user_id())
        .load()
        .await
        .map_err(bucket_error)?;
    Ok(edit_page(
        &auth,
        schema,
        &metadata,
        notices.get("saved").and_then(|v| v.as_bool()).unwrap_or_default(),
        "",
    ))
}

#[derive(Debug, thiserror::Error)]
enum MetadataFormError {
    #[error("送信内容を読み込めませんでした。")]
    Read(#[from] MultipartError),
    #[error("入力内容がサイズ上限を超えています。")]
    FieldTooLarge,
    #[error("送信項目が多すぎます。")]
    TooManyFields,
    #[error("項目名がありません。")]
    MissingFieldName,
    #[error("同じ項目が重複しています。")]
    DuplicateField,
    #[error("項目名が不正です。")]
    InvalidFieldName,
    #[error("文面はUTF-8で送信してください。")]
    InvalidUtf8(#[from] std::string::FromUtf8Error),
    #[error("削除指定が不正です。")]
    InvalidRemoval,
    #[error("同じ項目の変更と削除は同時に指定できません。")]
    ConflictingChanges,
    #[error(transparent)]
    Validation(#[from] MetadataValidationError),
}

async fn read_field(mut field: Field<'_>, limit: usize) -> Result<Bytes, MetadataFormError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = field.chunk().await? {
        if bytes.len() + chunk.len() > limit {
            return Err(MetadataFormError::FieldTooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes.into())
}

async fn parse_form(multipart: &mut Multipart, update: &mut MetadataUpdate) -> Result<(), MetadataFormError> {
    let mut seen = BTreeSet::new();
    let mut count = 0;
    while let Some(field) = multipart.next_field().await? {
        count += 1;
        if count > MAX_FIELDS * 2 {
            return Err(MetadataFormError::TooManyFields);
        }
        let name = field.name().ok_or(MetadataFormError::MissingFieldName)?.to_owned();
        if !seen.insert(name.clone()) {
            return Err(MetadataFormError::DuplicateField);
        }
        let (kind, key) = name.split_once('.').ok_or(MetadataFormError::InvalidFieldName)?;
        if !valid_field_key(key) {
            return Err(MetadataFormError::InvalidFieldName);
        }
        let change = match kind {
            "string" => {
                let text = String::from_utf8(read_field(field, MAX_TEXT_BYTES).await?.to_vec())?;
                MetadataChange::String(text.replace("\r\n", "\n").replace('\r', "\n"))
            }
            "file" => {
                let filename = field.file_name().unwrap_or_default().to_owned();
                let bytes = read_field(field, MAX_FILE_BYTES).await?;
                if filename.is_empty() && bytes.is_empty() {
                    continue;
                }
                MetadataChange::File(MetadataFile::new(bytes, filename)?)
            }
            "remove" => {
                if read_field(field, 4).await?.as_ref() != b"true" {
                    return Err(MetadataFormError::InvalidRemoval);
                }
                MetadataChange::Remove
            }
            _ => return Err(MetadataFormError::InvalidFieldName),
        };
        if update.0.contains_key(key) {
            return Err(MetadataFormError::ConflictingChanges);
        }
        update.0.insert(key.to_owned(), change);
    }
    update.validate()?;
    Ok(())
}

pub(super) async fn save<S: Config>(
    State(state): State<S>,
    Extension(auth): Extension<AuthState>,
    multipart: Result<Multipart, MultipartRejection>,
) -> Result<Response, (StatusCode, &'static str)> {
    if !auth.user.is_participant() {
        return Err((StatusCode::FORBIDDEN, "寄稿者権限が必要です。"));
    }
    let mut multipart = multipart.map_err(|_| (StatusCode::BAD_REQUEST, "フォームの送信形式が不正です。"))?;
    let mut update = MetadataUpdate::default();
    let bucket = state.bucket();
    let project = bucket.project(auth.project.id);
    let store = project.metadata_by_user(auth.user.user_id());
    if let Err(error) = parse_form(&mut multipart, &mut update).await {
        let mut metadata = store.load().await.map_err(bucket_error)?;
        for (key, change) in update.0 {
            if let MetadataChange::String(value) = change {
                metadata.values.insert(key, value);
            }
        }
        let schema = state.projects().metadata_schema(auth.project.id).await.map_err(storage_error)?;
        return Ok((
            StatusCode::UNPROCESSABLE_ENTITY,
            edit_page(&auth, schema, &metadata, false, &error.to_string()),
        )
            .into_response());
    }
    // Deliberately do not compare the submitted fields to the current schema.
    store.save(update).await.map_err(bucket_error)?;
    state.builds().request(auth.project.id).await.map_err(storage_error)?;
    Ok((
        Extension(Notices::from([("saved", true)])),
        Redirect::to(&format!("/{}/participant/metadata", auth.project.name)),
    )
        .into_response())
}

pub(super) async fn file<S: Config>(
    State(state): State<S>,
    Extension(auth): Extension<AuthState>,
    Path(key): Path<String>,
) -> Result<FileResponse, (StatusCode, &'static str)> {
    if !auth.user.is_participant() {
        return Err((StatusCode::FORBIDDEN, "寄稿者権限が必要です。"));
    }
    let (filename, data) = state
        .bucket()
        .project(auth.project.id)
        .metadata_by_user(auth.user.user_id())
        .file(&key)
        .await
        .map_err(bucket_error)?
        .ok_or((StatusCode::NOT_FOUND, "ファイルがありません。"))?;
    Ok(FileResponse {
        key: filename,
        data,
        attachment: true,
    })
}
