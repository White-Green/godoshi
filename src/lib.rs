use axum::routing::get;
use axum::Router;
use tower_service::Service;
use worker::{console_log, event, Context, Env, HttpRequest, MessageBatch};

fn router() -> Router {
    Router::new().route("/", get(root))
}

#[event(fetch)]
async fn fetch(req: HttpRequest, _env: Env, _ctx: Context) -> worker::Result<axum::http::Response<axum::body::Body>> {
    Ok(router().call(req).await?)
}

#[event(queue)]
async fn queue(message_batch: MessageBatch<()>, _env: Env, _ctx: Context) -> worker::Result<()> {
    for message in message_batch.messages()? {
        console_log!("Received message from queue {}: {:?}", message_batch.queue(), message.body());
    }
    Ok(())
}

pub async fn root() -> &'static str {
    "Hello Axum!"
}
