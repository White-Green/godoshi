use crate::auth::discord::AuthSettingProvider;
use crate::core::Config;
use crate::core::build::BuildRequest;
use crate::encrypted_cookie::{EncryptedCookieKey, EncryptedCookieKeyProvider};
use crate::http::HttpClient;
use crate::infrastructure::bucket::R2Bucket;
use crate::infrastructure::build::BuildQueue;
use crate::infrastructure::container;
use crate::infrastructure::d1::D1Store;
use crate::infrastructure::environment::{EnvironmentUploaded, R2EnvironmentUploads};
use crate::{app, auth, project, s3};
use axum::Router;
use axum::body::Body;
use axum::http::{Response, StatusCode};
use futures::TryStreamExt;
use s3s::{S3Error, S3ErrorCode};
use tower_service::Service;
use worker::{Context, Env, HttpRequest, MessageBatch, MessageExt, Method, Request, event};
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

impl Config for AppState {
    type Projects = D1Store;
    type Users = D1Store;
    type Bucket = R2Bucket;
    type Builds = BuildQueue;
    type EnvironmentUploads = R2EnvironmentUploads;

    fn environment_uploads(&self) -> R2EnvironmentUploads {
        R2EnvironmentUploads(self.env.clone())
    }

    fn builds(&self) -> BuildQueue {
        BuildQueue {
            db: self.env.d1("DB").expect("DB must be configured"),
            queue: self.env.queue("JOB_QUEUE").expect("JOB_QUEUE must be configured"),
        }
    }
    fn base_url(&self) -> String {
        self.env
            .var("BASE_URL")
            .inspect_err(|_| tracing::error!(binding = "BASE_URL", "Required configuration unavailable"))
            .expect("BASE_URL must be configured")
            .to_string()
    }

    fn projects(&self) -> D1Store {
        D1Store(self.env.d1("DB").unwrap())
    }

    fn users(&self) -> D1Store {
        self.projects()
    }

    fn bucket(&self) -> R2Bucket {
        R2Bucket::new(
            self.env.bucket("BUCKET").expect("BUCKET must be configured"),
            self.env.d1("DB").expect("DB must be configured"),
        )
    }
}

impl HttpClient for AppState {
    type Error = std::io::Error;

    #[worker::send]
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
            .expect("Application-generated HTTP requests must be valid Workers requests");
        let response = worker::Fetch::Request(request)
            .send()
            .await
            .inspect_err(|error| tracing::error!(%error, stage = "send", "External request failed"))
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
            .inspect_err(|error| tracing::error!(%error, stage = "response_body", "External request failed"))
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

#[cfg_attr(feature = "ui-preview", allow(dead_code))]
fn router(env: Env) -> Router {
    let state = AppState { env };
    let auth_layer = axum::middleware::from_fn_with_state(state.clone(), auth::middleware::<AppState>);
    let project_layer = axum::middleware::from_fn_with_state(state.clone(), project::middleware::<AppState>);
    Router::new()
        .nest("/auth", auth::route())
        .nest("/upload", app::upload::route())
        .nest_service(
            "/{project}",
            app::route(state.clone())
                .fallback(async || StatusCode::NOT_FOUND)
                .layer(tower::builder::ServiceBuilder::new().layer(project_layer).layer(auth_layer)),
        )
        .with_state(state)
}

#[event(fetch)]
#[tracing::instrument(name = "http_request", skip_all, fields(method = %req.method(), path = request_log_path(req.uri().path()), request_id = req.headers().get("cf-ray").and_then(|value| value.to_str().ok())), err)]
async fn fetch(req: HttpRequest, env: Env, _ctx: Context) -> worker::Result<axum::http::Response<axum::body::Body>> {
    #[cfg(feature = "worker-tests")]
    if req.method() == axum::http::Method::POST && req.uri().path() == "/test" {
        return Ok(crate::worker_tests::run(env).await);
    }

    let started = time::OffsetDateTime::now_utc();
    let response = cfg_select! {
        feature = "ui-preview" => {{
            let _ = env;
            crate::ui_preview::router().await.call(req).await?
        }}
        _ => router(env).call(req).await?,
    };
    let status = response.status().as_u16();
    let elapsed_ms = (time::OffsetDateTime::now_utc() - started).as_seconds_f64() * 1000.0;
    if response.status().is_server_error() {
        tracing::error!(status, elapsed_ms, "Response prepared");
    } else {
        tracing::info!(status, elapsed_ms, "Response prepared");
    }
    Ok(response)
}

fn request_log_path(path: &str) -> &str {
    // Upload URLs are bearer credentials; do not include them in application logs.
    if path.starts_with("/upload/") { "/upload/[redacted]" } else { path }
}

#[event(fetch, entrypoint = "OutboundHandler")]
#[tracing::instrument(name = "outbound_request", skip_all, fields(method = %req.method(), path = req.path(), project_id, run_id), err)]
async fn outbound_handler(req: Request, env: Env, ctx: Context) -> worker::Result<worker::Response> {
    let is_head = req.method() == Method::Head;
    let result = async {
        let params = ctx.props::<s3::Params>().map_err(|_| {
            tracing::warn!(reason = "invalid_s3_context", "S3 request rejected");
            S3Error::new(S3ErrorCode::AccessDenied)
        })?;
        tracing::Span::current().record("project_id", tracing::field::display(params.project_id));
        tracing::Span::current().record("run_id", &params.run_id);
        let r2 = env.bucket("BUCKET").map_err(|error| {
            tracing::error!(%error, binding = "BUCKET", "Required binding unavailable");
            S3Error::new(S3ErrorCode::InternalError)
        })?;
        let db = env.d1("DB").map_err(|error| {
            tracing::error!(%error, binding = "DB", "Required binding unavailable");
            S3Error::new(S3ErrorCode::InternalError)
        })?;
        let request = axum::http::Request::try_from(req)
            .map_err(|error| {
                tracing::error!(%error, "S3 request conversion failed");
                S3Error::new(S3ErrorCode::InternalError)
            })?
            .map(s3s::Body::http_body);
        s3::handle(request, &R2Bucket::new(r2, db), params).await
    }
    .await;
    let response = match result {
        Ok(response) => response,
        Err(error) => error.to_http_response().map_err(|error| worker::Error::RustError(error.to_string()))?,
    };
    let mut response = worker::Response::try_from(response)?.with_encode_body(worker::EncodeBody::Manual);
    if is_head {
        response = response.into_parts().0.empty();
    }
    let status = response.status_code();
    if status >= 500 {
        tracing::error!(status, "S3 response prepared");
    } else if matches!(status, 401 | 403) {
        tracing::warn!(status, reason = "access_denied", "S3 request rejected");
    }
    Ok(response)
}

#[derive(serde::Deserialize)]
#[serde(untagged)]
enum QueueMessage {
    Build(BuildRequest),
    Environment(EnvironmentUploaded),
}

async fn process_queue_message(message: &QueueMessage, env: &Env) -> worker::Result<()> {
    match message {
        QueueMessage::Build(job) => container::run(env, job).await,
        QueueMessage::Environment(event) => {
            let state = AppState { env: env.clone() };
            event
                .request_build(
                    &env.var("R2_ACCOUNT_ID")?.to_string(),
                    &env.var("R2_BUCKET_NAME")?.to_string(),
                    &state.builds(),
                    &state.bucket(),
                )
                .await
                .map_err(|error| worker::Error::RustError(error.to_string()))
        }
    }
}

#[event(queue)]
#[tracing::instrument(name = "queue", skip_all, fields(queue = %message_batch.queue()), err)]
async fn queue(message_batch: MessageBatch<QueueMessage>, env: Env, _ctx: Context) -> worker::Result<()> {
    for message in message_batch.messages()? {
        let message_id = message.id();
        // Waiting for an earlier build plus this build must fit the 15-minute consumer limit.
        match process_queue_message(message.body(), &env).await {
            Ok(()) => {
                message.ack();
                tracing::info!(message_id, "Queue message acknowledged");
            }
            Err(error) => {
                match message.body() {
                    QueueMessage::Build(job) => {
                        tracing::error!(message_id, project_id = %job.project_id, revision = job.revision, %error, "Build queue message failed; retrying")
                    }
                    QueueMessage::Environment(_) => tracing::error!(message_id, %error, "Environment queue message failed; retrying"),
                }
                message.retry();
            }
        }
    }
    Ok(())
}
