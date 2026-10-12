use crate::config::GenericConfigProvider;
use askama::Template;
use axum::Router;
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{Html, IntoResponse, Response};
use oauth2::url::Url;

mod admin;
mod participant;
mod results;
mod review;
mod root;
mod template;

#[cfg(feature = "worker-tests")]
pub mod tests;

pub fn route<S>(state: S) -> Router<()>
where
    S: 'static + Send + Sync + Clone + GenericConfigProvider,
{
    Router::new()
        .route("/", axum::routing::get(root::index))
        .merge(admin::route::<S>())
        .merge(review::route::<S>())
        .merge(participant::route::<S>())
        .layer(axum::middleware::from_fn_with_state(state.clone(), protect::<S>))
        .with_state(state)
}

async fn protect<S: GenericConfigProvider>(State(state): State<S>, request: Request, next: Next) -> Response {
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

fn storage_error(error: worker::Error) -> (StatusCode, &'static str) {
    tracing::error!(%error, "Page data operation failed");
    (StatusCode::INTERNAL_SERVER_ERROR, "処理に失敗しました。時間をおいて再度お試しください。")
}
