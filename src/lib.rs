use axum::extract::State as AxumState;
use axum::http::StatusCode;
use axum::routing::get;
use axum::Router;
use tower_service::Service;
use worker::wasm_bindgen::JsCast;
use worker::{
    console_log, durable_object, event, Context, DurableObject, Env, Fetcher, HttpRequest, MessageBatch, Method, Request, Response, Result, State,
};

#[durable_object]
pub struct MyContainer {
    state: State,
    #[allow(dead_code)]
    env: Env,
}

impl DurableObject for MyContainer {
    fn new(state: State, env: Env) -> Self {
        Self { state, env }
    }

    async fn fetch(&self, req: Request) -> Result<Response> {
        if let Some(container) = self.state.container() {
            if let Ok(binding) = self.state.exports().get("OutboundHandler") {
                let outbound_fetcher = Fetcher::unchecked_from_js(binding);
                let _ = container.intercept_all_outbound_http(&outbound_fetcher).await;
            }
            if !container.running() {
                container.start(None)?;
            }
            let port = container.get_tcp_port(8080)?;
            let res = port.fetch_request(req).await?;
            Response::try_from(res)
        } else {
            Response::ok("Hello from Container Durable Object!")
        }
    }
}

#[derive(Clone)]
struct AppState {
    env: Env,
}

fn router(env: Env) -> Router {
    Router::new()
        .route("/", get(root))
        .route("/container", get(container_handler))
        .route("/container/outbound", get(container_outbound_handler))
        .with_state(AppState { env })
}

#[event(fetch)]
async fn fetch(req: HttpRequest, env: Env, _ctx: Context) -> worker::Result<axum::http::Response<axum::body::Body>> {
    Ok(router(env).call(req).await?)
}

#[event(fetch, entrypoint = "OutboundHandler")]
async fn outbound_handler(req: HttpRequest, _env: Env, _ctx: Context) -> worker::Result<axum::http::Response<axum::body::Body>> {
    console_log!("OutboundHandler intercepted request: uri={}", req.uri());
    let body = format!("Hello from OutboundHandler! Intercepted request to: {}", req.uri());
    Ok(axum::http::Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "text/plain")
        .body(axum::body::Body::from(body))
        .unwrap())
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

#[worker::send]
async fn container_handler(AxumState(state): AxumState<AppState>) -> std::result::Result<String, (StatusCode, String)> {
    fetch_container(&state.env, "http://container/").await
}

#[worker::send]
async fn container_outbound_handler(AxumState(state): AxumState<AppState>) -> std::result::Result<String, (StatusCode, String)> {
    fetch_container(&state.env, "http://container/outbound").await
}

async fn fetch_container(env: &Env, url: &str) -> std::result::Result<String, (StatusCode, String)> {
    let namespace = env
        .durable_object("CONTAINER")
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let stub = namespace
        .id_from_name("default")
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .get_stub()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let req = Request::new(url, Method::Get).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let mut response = stub
        .fetch_with_request(req)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let text = response.text().await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(text)
}
