use crate::core::bucket::{Bucket, generate_key};
use crate::core::build::BuildRequest;
use crate::core::operation_log::{OperationLog, record};
use crate::core::project::ProjectId;
use crate::infrastructure::bucket::R2Bucket;
use crate::s3;
use coordinator::{BuildTarget, Coordinator};
use wasm_bindgen::JsCast;
use worker::{D1Database, DurableObject, Env, Fetcher, Method, Request, RequestInit, Response, State, durable_object};

mod coordinator;

const S3_HOST: &str = "s3.internal";

#[durable_object]
pub struct MyContainer {
    state: State,
    env: Env,
    coordinator: Coordinator,
}

impl DurableObject for MyContainer {
    fn new(state: State, env: Env) -> Self {
        Self {
            state,
            env,
            coordinator: Coordinator::default(),
        }
    }

    #[tracing::instrument(name = "build_container_request", skip_all, fields(project_id, revision), err)]
    async fn fetch(&self, mut req: Request) -> worker::Result<Response> {
        if req.path() != "/run" {
            return Response::error("Not found", 404);
        }
        if req.method() != Method::Post {
            return Response::error("Method not allowed", 405);
        }
        let job: BuildRequest = match req.json().await {
            Ok(job) => job,
            Err(_) => return Response::error("A numeric project_id and revision are required", 400),
        };
        tracing::Span::current().record("project_id", tracing::field::display(job.project_id));
        tracing::Span::current().record("revision", job.revision);
        self.coordinator.run(self, &job).await?;
        Response::empty()
    }
}

impl MyContainer {
    fn container(&self) -> worker::Result<worker::Container> {
        self.state
            .container()
            .ok_or_else(|| worker::Error::RustError("CONTAINER has no container configured".into()))
    }
}

impl BuildTarget for MyContainer {
    type Error = worker::Error;

    async fn pending(&self, job: &BuildRequest) -> worker::Result<bool> {
        pending(&self.env.d1("DB")?, job).await
    }

    async fn wait_for_previous(&self) -> worker::Result<()> {
        let container = self.container()?;
        if container.running() {
            let exit_code = container_exit_code(container.wait_for_exit().await)?;
            if exit_code != 0 {
                tracing::warn!(exit_code, "Previous build command failed; continuing with the next request");
            }
        }
        Ok(())
    }

    async fn build_if_pending(&self, job: &BuildRequest) -> worker::Result<()> {
        let db = self.env.d1("DB")?;
        if !pending(&db, job).await? {
            return Ok(());
        }
        let run_id = generate_key(time::OffsetDateTime::now_utc()).map_err(|error| worker::Error::RustError(error.to_string()))?;
        let bucket = R2Bucket::new(self.env.bucket("BUCKET")?, self.env.d1("DB")?);
        let project = bucket.project(job.project_id);
        let params = s3::Params {
            project_id: job.project_id,
            run_id: run_id.as_str().to_owned(),
        };
        let container = self.container()?;
        let outbound = Fetcher::unchecked_from_js(self.state.exports().get_with_props("OutboundHandler", &params)?);
        container.intercept_outbound_http(S3_HOST, &outbound).await?;
        if !claim(&db, job).await? {
            return Ok(());
        }
        if let Err(error) = container.start(None) {
            tracing::error!(project_id = %job.project_id, revision = job.revision, run_id = params.run_id, %error, "Build container start failed");
            record(&project, OperationLog::BuildFailed { run_id, exit_code: None }).await;
            release(&db, job).await?;
            return Err(error);
        }
        tracing::info!(project_id = %job.project_id, revision = job.revision, run_id = params.run_id, "Build container started");
        // Start monitoring immediately, even while the log is being persisted.
        let (_, result) = futures::join!(
            record(&project, OperationLog::BuildStarted { run_id: run_id.clone() }),
            container.wait_for_exit(),
        );
        match container_exit_code(result) {
            Ok(0) => {
                record(&project, OperationLog::BuildSucceeded { run_id }).await;
                tracing::info!(project_id = %job.project_id, revision = job.revision, run_id = params.run_id, "Build container completed");
            }
            Ok(exit_code) => {
                record(
                    &project,
                    OperationLog::BuildFailed {
                        run_id,
                        exit_code: Some(exit_code),
                    },
                )
                .await;
                tracing::warn!(project_id = %job.project_id, revision = job.revision, run_id = params.run_id, exit_code, "Build command failed");
            }
            Err(error) => {
                record(&project, OperationLog::BuildFailed { run_id, exit_code: None }).await;
                tracing::error!(project_id = %job.project_id, revision = job.revision, run_id = params.run_id, %error, "Build container lifecycle failed");
                release(&db, job).await?;
                return Err(error);
            }
        }
        Ok(())
    }
}

fn container_exit_code(result: worker::Result<()>) -> worker::Result<i32> {
    match result {
        Ok(()) => Ok(0),
        Err(error) => {
            // monitor() marks non-zero process exits with exitCode; other failures remain errors.
            // https://developers.cloudflare.com/containers/api/durable-object-container/#monitor
            worker::js_sys::Reflect::get(&error.to_js_value(), &"exitCode".into())
                .ok()
                .and_then(|value| value.as_f64())
                .filter(|code| *code != 0.0 && code.fract() == 0.0 && (i32::MIN as f64..=i32::MAX as f64).contains(code))
                .map(|code| code as i32)
                .ok_or(error)
        }
    }
}

pub async fn run(env: &Env, job: &BuildRequest) -> worker::Result<()> {
    if !pending(&env.d1("DB")?, job).await? {
        return Ok(());
    }
    let namespace = env.durable_object("CONTAINER")?;
    let stub = namespace.id_from_name(&format!("project:{}", job.project_id))?.get_stub()?;
    let request = Request::new_with_init(
        "http://container/run",
        RequestInit::new()
            .with_method(Method::Post)
            .with_body(Some(serde_json::to_string(job)?.into())),
    )?;
    let response = stub.fetch_with_request(request).await?;
    if !(200..300).contains(&response.status_code()) {
        return Err(worker::Error::RustError(format!("Container returned HTTP {}", response.status_code())));
    }
    Ok(())
}

pub(super) async fn pending(db: &D1Database, job: &BuildRequest) -> worker::Result<bool> {
    Ok(worker::query!(
        db,
        "SELECT id FROM project WHERE id = ? AND build_revision = ? AND started_build_revision < build_revision",
        job.project_id,
        job.revision
    )?
    .first::<ProjectId>(Some("id"))
    .await?
    .is_some())
}

pub(super) async fn claim(db: &D1Database, job: &BuildRequest) -> worker::Result<bool> {
    Ok(worker::query!(
        db,
        "UPDATE project SET started_build_revision = build_revision
         WHERE id = ? AND build_revision = ? AND started_build_revision < build_revision RETURNING id",
        job.project_id,
        job.revision
    )?
    .first::<ProjectId>(Some("id"))
    .await?
    .is_some())
}

pub(super) async fn release(db: &D1Database, job: &BuildRequest) -> worker::Result<()> {
    worker::query!(
        db,
        "UPDATE project SET started_build_revision = ? WHERE id = ? AND started_build_revision = ?",
        job.revision - 1,
        job.project_id,
        job.revision
    )?
    .run()
    .await?;
    Ok(())
}
