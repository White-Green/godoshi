use crate::auth::discord::AuthSettingProvider;
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
use worker::{Context, Env, HttpRequest, MessageBatch, Method, Request, console_log, event};

mod app;
mod auth;
mod encrypted_cookie;
mod http;
mod project;

const HOST: &str = "godoshi.white-green.net";

// #[durable_object]
// pub struct MyContainer {
//     state: State,
//     env: Env,
// }
//
// impl DurableObject for MyContainer {
//     fn new(state: State, env: Env) -> Self {
//         Self { state, env }
//     }
//
//     async fn fetch(&self, _req: Request) -> Result<Response> {
//         todo!()
//         // if let Some(container) = self.state.container() {
//         //     if let Ok(binding) = self.state.exports().get("OutboundHandler") {
//         //         let outbound_fetcher = Fetcher::unchecked_from_js(binding);
//         //         let _ = container.intercept_all_outbound_http(&outbound_fetcher).await;
//         //     }
//         //     if !container.running() {
//         //         container.start(None)?;
//         //     }
//         //     let port = container.get_tcp_port(8080)?;
//         //     let res = port.fetch_request(req).await?;
//         //     Response::try_from(res)
//         // } else {
//         //     Response::ok("Hello from Container Durable Object!")
//         // }
//     }
// }

#[derive(Clone)]
struct AppState {
    env: Env,
}

impl EncryptedCookieKeyProvider for AppState {
    fn encrypted_cookie_key(&self) -> EncryptedCookieKey {
        let key = self.env.secret("COOKIE_KEY").unwrap().to_string();
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
        self.env.secret("DISCORD_CLIENT_ID").unwrap().to_string()
    }

    fn client_secret(&self) -> String {
        self.env.secret("DISCORD_CLIENT_SECRET").unwrap().to_string()
    }
}

impl HttpClient for AppState {
    type Error = std::io::Error;

    async fn call(&self, request: axum::http::Request<Vec<u8>>) -> Result<Response<Vec<u8>>, Self::Error> {
        let (parts, body) = request.into_parts();
        let request = axum::http::Request::<Body>::from_parts(parts, Body::from(body));
        let response = worker::Fetch::Request(request.try_into().map_err(std::io::Error::other)?)
            .send()
            .await
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
            .map_err(std::io::Error::other)?;

        Ok(Response::from_parts(parts, body))
    }
}

fn router(env: Env) -> Router {
    let state = AppState { env };
    let auth_layer = axum::middleware::from_fn_with_state(state.clone(), auth::middleware::<AppState>);
    let project_layer = axum::middleware::from_fn_with_state(state.clone(), project::middleware::<AppState>);
    Router::new()
        .nest("/auth", auth::route())
        .nest(
            "/:project",
            app::route().layer(tower::builder::ServiceBuilder::new().layer(auth_layer).layer(project_layer)),
        )
        .route("/", get(root))
        .route("/container", get(container_handler))
        .route("/container/outbound", get(container_outbound_handler))
        .with_state(state)
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
