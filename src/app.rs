use axum::Router;

pub fn route<S>() -> Router<S>
where
    S: 'static + Send + Sync + Clone,
{
    Router::new().route("/", axum::routing::get(async || "Hello, World!"))
}
