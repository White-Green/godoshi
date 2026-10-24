use crate::core::build::{BuildRequest, BuildRequests};
use crate::core::project::ProjectId;
use worker::{D1Database, MessageBuilder, Queue};

pub const DEBOUNCE_SECONDS: u32 = 60;

pub struct BuildQueue {
    pub db: D1Database,
    pub queue: Queue,
}

impl BuildRequests for BuildQueue {
    type Error = std::io::Error;

    #[worker::send]
    async fn request(&self, project: ProjectId) -> Result<(), Self::Error> {
        let job = increment(&self.db, project).await.map_err(error)?;
        self.queue
            .send(MessageBuilder::new(job).delay_seconds(DEBOUNCE_SECONDS).build())
            .await
            .map_err(error)?;
        tracing::info!(project_id = %project, revision = job.revision, delay_seconds = DEBOUNCE_SECONDS, "Build requested");
        Ok(())
    }
}

fn error(error: worker::Error) -> std::io::Error {
    std::io::Error::other(error.to_string())
}

async fn increment(db: &D1Database, project: ProjectId) -> worker::Result<BuildRequest> {
    let revision = worker::query!(
        db,
        "UPDATE project SET build_revision = build_revision + 1 WHERE id = ? RETURNING build_revision",
        project
    )?
    .first::<u64>(Some("build_revision"))
    .await?
    .ok_or_else(|| worker::Error::RustError("Build project does not exist".into()))?;
    Ok(BuildRequest {
        project_id: project,
        revision,
    })
}
