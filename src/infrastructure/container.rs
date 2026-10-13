use crate::core::bucket::ResultKey;
use crate::s3;
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;
use worker::{DurableObject, Env, Fetcher, Method, Request, RequestInit, Response, State, durable_object};

const S3_HOST: &str = "s3.internal";

/// Parameters for a container run. The project determines the outbound S3 scope.
#[derive(Deserialize, Serialize)]
pub struct RunRequest {
    pub project_id: u64,
}

#[durable_object]
pub struct MyContainer {
    state: State,
}

impl DurableObject for MyContainer {
    fn new(state: State, _env: Env) -> Self {
        Self { state }
    }

    async fn fetch(&self, mut req: Request) -> worker::Result<Response> {
        if req.path() != "/run" {
            return Response::error("Not found", 404);
        }
        if req.method() != Method::Post {
            return Response::error("Method not allowed", 405);
        }
        let job: s3::Params = match req.json().await {
            Ok(job) => job,
            Err(_) => return Response::error("A numeric project_id and run_id are required", 400),
        };

        let container = self
            .state
            .container()
            .ok_or_else(|| worker::Error::RustError("CONTAINER has no container configured".into()))?;
        let outbound = Fetcher::unchecked_from_js(self.state.exports().get_with_props("OutboundHandler", &job)?);
        // Install the project-scoped S3 intercept before startup downloads the closure.
        container.intercept_outbound_http(S3_HOST, &outbound).await?;
        if !container.running() {
            container.start(None)?;
            tracing::info!(project_id = job.project_id, run_id = job.run_id, "Build container started");
        }

        // Startup errors and non-zero exits reject monitor.
        container.wait_for_exit().await?;
        tracing::info!(project_id = job.project_id, run_id = job.run_id, "Build container completed");
        Response::empty()
    }
}

pub async fn run(env: &Env, name: &str, params: &RunRequest, received_at: time::OffsetDateTime) -> worker::Result<()> {
    let mut random = [0; 8];
    getrandom::fill(&mut random).map_err(|error| worker::Error::RustError(error.to_string()))?;
    let params = s3::Params {
        project_id: params.project_id,
        // Newer runs sort first when R2 lists keys in ascending order.
        run_id: ResultKey::for_build(received_at, u64::from_le_bytes(random)).as_str().to_owned(),
    };
    let namespace = env.durable_object("CONTAINER")?;
    let stub = namespace.id_from_name(name)?.get_stub()?;
    let body = serde_json::to_string(&params)?;
    let request = Request::new_with_init(
        "http://container/run",
        RequestInit::new().with_method(Method::Post).with_body(Some(body.into())),
    )?;
    let response = stub.fetch_with_request(request).await?;
    if !(200..300).contains(&response.status_code()) {
        return Err(worker::Error::RustError(format!("Container returned HTTP {}", response.status_code())));
    }
    Ok(())
}
