use crate::auth::discord::AuthSettingProvider;
use crate::config::BaseUrlProvider;
use crate::encrypted_cookie::{EncryptedCookieKey, EncryptedCookieKeyProvider};
use crate::http::HttpClient;
use crate::project::{ProjectManager, ProjectName};
use axum::Router;
use axum::body::Body;
use axum::extract::State as AxumState;
use axum::http::{Response, StatusCode};
use axum::routing::get;
use futures::TryStreamExt;
use tower_service::Service;
use worker::{Context, DurableObject, Env, HttpRequest, MessageBatch, MessageExt, Method, Request, State, durable_object, event};

mod app;
mod auth;
mod config;
mod encrypted_cookie;
mod http;
mod project;

#[event(start)]
fn start() {
    // The start event runs once per isolate, including Durable Object and named entrypoints.
    tracing_subscriber::fmt()
        .json()
        .with_ansi(false)
        .without_time() // Workers supplies timestamps; std::time is unavailable on this target.
        .with_max_level(tracing::Level::INFO)
        .with_writer(tracing_web::MakeWebConsoleWriter::new())
        .init();
}

#[durable_object]
#[allow(unused)]
pub struct MyContainer {
    state: State,
    env: Env,
}

impl DurableObject for MyContainer {
    fn new(state: State, env: Env) -> Self {
        Self { state, env }
    }

    async fn fetch(&self, _req: Request) -> Result<worker::Response, worker::Error> {
        tracing::error!("Container Durable Object fetch is not implemented");
        todo!()
        // if let Some(container) = self.state.container() {
        //     if let Ok(binding) = self.state.exports().get("OutboundHandler") {
        //         let outbound_fetcher = Fetcher::unchecked_from_js(binding);
        //         let _ = container.intercept_all_outbound_http(&outbound_fetcher).await;
        //     }
        //     if !container.running() {
        //         container.start(None)?;
        //     }
        //     let port = container.get_tcp_port(8080)?;
        //     let res = port.fetch_request(req).await?;
        //     Response::try_from(res)
        // } else {
        //     Response::ok("Hello from Container Durable Object!")
        // }
    }
}

#[derive(Clone)]
struct AppState {
    env: Env,
}

impl EncryptedCookieKeyProvider for AppState {
    fn encrypted_cookie_key(&self) -> EncryptedCookieKey {
        let key = self
            .env
            .secret("COOKIE_KEY")
            .inspect_err(|_| tracing::error!(binding = "COOKIE_KEY", "Required secret unavailable"))
            .unwrap()
            .to_string();
        EncryptedCookieKey::from_bytes(key.as_bytes())
    }
}

impl ProjectManager for AppState {
    fn project_exists(&self, project: &ProjectName) -> bool {
        project == "test"
    }
}

impl AuthSettingProvider for AppState {
    fn client_id(&self) -> String {
        self.env
            .secret("DISCORD_CLIENT_ID")
            .inspect_err(|_| tracing::error!(binding = "DISCORD_CLIENT_ID", "Required secret unavailable"))
            .unwrap()
            .to_string()
    }

    fn client_secret(&self) -> String {
        self.env
            .secret("DISCORD_CLIENT_SECRET")
            .inspect_err(|_| tracing::error!(binding = "DISCORD_CLIENT_SECRET", "Required secret unavailable"))
            .unwrap()
            .to_string()
    }
}

impl BaseUrlProvider for AppState {
    fn base_url(&self) -> String {
        self.env
            .var("BASE_URL")
            .inspect_err(|_| tracing::error!(binding = "BASE_URL", "Required configuration unavailable"))
            .expect("BASE_URL must be configured")
            .to_string()
    }
}

impl HttpClient for AppState {
    type Error = std::io::Error;

    #[tracing::instrument(
        name = "http_client",
        skip_all,
        fields(method = %request.method(), host = request.uri().host().unwrap_or_default(), path = request.uri().path())
    )]
    async fn call(&self, request: axum::http::Request<Vec<u8>>) -> Result<Response<Vec<u8>>, Self::Error> {
        let started = time::OffsetDateTime::now_utc();
        let (parts, body) = request.into_parts();
        let request = axum::http::Request::<Body>::from_parts(parts, Body::from(body));
        let request = request
            .try_into()
            .inspect_err(|_| tracing::error!(stage = "request_conversion", "External request failed"))
            .map_err(std::io::Error::other)?;
        let response = worker::Fetch::Request(request)
            .send()
            .await
            .inspect_err(|_| tracing::error!(stage = "send", "External request failed"))
            .map_err(std::io::Error::other)?;
        let response = axum::http::Response::<Body>::from(response);
        let (parts, body) = response.into_parts();
        let body = body
            .into_data_stream()
            .try_fold(Vec::new(), |mut acc, chunk| {
                acc.extend(chunk);
                std::future::ready(Ok(acc))
            })
            .await
            .inspect_err(|_| tracing::error!(stage = "response_body", "External request failed"))
            .map_err(std::io::Error::other)?;

        let status = parts.status.as_u16();
        let elapsed_ms = (time::OffsetDateTime::now_utc() - started).as_seconds_f64() * 1000.0;
        if parts.status.is_server_error() {
            tracing::error!(status, elapsed_ms, "External request completed");
        } else if parts.status.is_client_error() {
            tracing::warn!(status, elapsed_ms, "External request completed");
        } else {
            tracing::info!(status, elapsed_ms, "External request completed");
        }
        Ok(Response::from_parts(parts, body))
    }
}

fn router(env: Env) -> Router {
    let state = AppState { env };
    let auth_layer = axum::middleware::from_fn_with_state(state.clone(), auth::middleware::<AppState>);
    let project_layer = axum::middleware::from_fn_with_state(state.clone(), project::middleware::<AppState>);
    Router::new()
        .nest("/auth", auth::route())
        .nest_service(
            "/{project}",
            app::route()
                .fallback(async || StatusCode::NOT_FOUND)
                .layer(tower::builder::ServiceBuilder::new().layer(project_layer).layer(auth_layer)),
        )
        .route("/", get(root))
        .route("/container", get(container_handler))
        .route("/container/outbound", get(container_outbound_handler))
        .with_state(state)
}

#[event(fetch)]
#[tracing::instrument(name = "http_request", skip_all, fields(method = %req.method(), path = req.uri().path()), err)]
async fn fetch(req: HttpRequest, env: Env, _ctx: Context) -> worker::Result<axum::http::Response<axum::body::Body>> {
    let started = time::OffsetDateTime::now_utc();
    let response = router(env).call(req).await?;
    let status = response.status().as_u16();
    let elapsed_ms = (time::OffsetDateTime::now_utc() - started).as_seconds_f64() * 1000.0;
    if response.status().is_server_error() {
        tracing::error!(status, elapsed_ms, "Request completed");
    } else {
        tracing::info!(status, elapsed_ms, "Request completed");
    }
    Ok(response)
}

#[event(fetch, entrypoint = "OutboundHandler")]
#[tracing::instrument(name = "outbound_request", skip_all, fields(method = %req.method(), path = req.uri().path()), err)]
async fn outbound_handler(req: HttpRequest, _env: Env, _ctx: Context) -> worker::Result<axum::http::Response<axum::body::Body>> {
    tracing::info!("OutboundHandler intercepted request");
    let body = format!("Hello from OutboundHandler! Intercepted request to: {}", req.uri());
    Ok(axum::http::Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "text/plain")
        .body(axum::body::Body::from(body))
        .unwrap())
}

#[event(queue)]
#[tracing::instrument(name = "queue", skip_all, fields(queue = %message_batch.queue()), err)]
async fn queue(message_batch: MessageBatch<()>, _env: Env, _ctx: Context) -> worker::Result<()> {
    let messages = message_batch.messages()?;
    let message_count = messages.len();
    tracing::info!(message_count, "Processing queue batch");
    for message in messages {
        tracing::debug!(message_id = %message.id(), "Received message from queue");
    }
    tracing::info!(message_count, "Queue batch completed");
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

#[tracing::instrument(name = "container_request", skip_all)]
async fn fetch_container(env: &Env, url: &str) -> std::result::Result<String, (StatusCode, String)> {
    let error_response = |stage: &'static str, error: worker::Error| {
        tracing::error!(stage, "Container request failed");
        (StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
    };
    let namespace = env.durable_object("CONTAINER").map_err(|e| error_response("binding", e))?;
    let stub = namespace
        .id_from_name("default")
        .map_err(|e| error_response("object_id", e))?
        .get_stub()
        .map_err(|e| error_response("stub", e))?;
    let req = Request::new(url, Method::Get).map_err(|e| error_response("request_creation", e))?;
    let mut response = stub.fetch_with_request(req).await.map_err(|e| error_response("send", e))?;
    let status = response.status_code();
    let text = response.text().await.map_err(|e| error_response("response_body", e))?;
    if status >= 500 {
        tracing::error!(status, "Container request completed");
    } else if status >= 400 {
        tracing::warn!(status, "Container request completed");
    } else {
        tracing::info!(status, "Container request completed");
    }
    Ok(text)
}
