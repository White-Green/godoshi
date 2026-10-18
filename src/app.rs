use crate::core::Config;
use askama::Template;
use axum::Router;
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{Html, IntoResponse, Response};
use oauth2::url::Url;
use serde::Deserialize;
use std::fmt::Display;

mod admin;
mod notice;
mod participant;
mod review;
mod root;
mod template;
pub mod upload;

pub fn route<S>(state: S) -> Router<()>
where
    S: 'static + Send + Sync + Clone + Config,
{
    Router::new()
        .route("/", axum::routing::get(root::index))
        .merge(admin::route::<S>())
        .merge(review::route::<S>())
        .merge(participant::route::<S>())
        .layer(axum::middleware::from_fn(notice::middleware))
        .layer(axum::middleware::from_fn_with_state(state.clone(), protect::<S>))
        .with_state(state)
}

async fn protect<S: Config>(State(state): State<S>, request: Request, next: Next) -> Response {
    if !matches!(*request.method(), axum::http::Method::GET | axum::http::Method::HEAD) {
        let origin = request.headers().get(header::ORIGIN).or_else(|| request.headers().get(header::REFERER));
        let origin = origin.and_then(|value| value.to_str().ok()).and_then(|value| Url::parse(value).ok());
        let expected = Url::parse(&state.base_url()).expect("BASE_URL must be a valid URL");
        if origin.is_none_or(|origin| origin.origin() != expected.origin()) {
            return (StatusCode::FORBIDDEN, "ページを開き直して操作してください。").into_response();
        }
    }
    let mut response = next.run(request).await;
    response.headers_mut().insert(header::CACHE_CONTROL, "private, no-store".parse().unwrap());
    response
}

fn render(title: &str, inner: impl Template) -> Response {
    Html(template::HtmlTemplate::new(title, inner).render().expect("Page template must render")).into_response()
}

fn storage_error(error: impl Display) -> (StatusCode, &'static str) {
    tracing::error!(%error, "Page data operation failed");
    (StatusCode::INTERNAL_SERVER_ERROR, "処理に失敗しました。時間をおいて再度お試しください。")
}

#[derive(serde::Deserialize)]
struct RunPath {
    run_id: crate::core::bucket::ResultKey,
}

#[derive(Deserialize)]
struct FilePath {
    run_id: crate::core::bucket::ResultKey,
    #[serde(deserialize_with = "file_key")]
    key: String,
}

fn file_key<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let key = String::deserialize(deserializer)?;
    if !crate::core::bucket::valid_key(&key) {
        return Err(serde::de::Error::custom("invalid file key"));
    }
    Ok(key)
}

fn bucket_error(error: crate::core::bucket::BucketError) -> (StatusCode, &'static str) {
    match error {
        crate::core::bucket::BucketError::InvalidKey => (StatusCode::BAD_REQUEST, "ファイル名または実行IDが不正です。"),
        crate::core::bucket::BucketError::InvalidFile => (StatusCode::BAD_REQUEST, "ファイルの一覧が不正です。"),
        crate::core::bucket::BucketError::TooManyFiles => (StatusCode::BAD_REQUEST, "ファイルは100個以下にしてください。"),
        crate::core::bucket::BucketError::Conflict => (StatusCode::CONFLICT, "ファイルの一覧が更新されています。ページを開き直してください。"),
        crate::core::bucket::BucketError::Read(_) => (StatusCode::BAD_REQUEST, "アップロードしたファイルを読み込めませんでした。"),
        crate::core::bucket::BucketError::Backend(error) => storage_error(error),
    }
}
