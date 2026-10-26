use super::project::ProjectId;
use serde::Serialize;
use std::error::Error;

pub const ENVIRONMENT_FILE: &str = "environment.closure_zstd";

#[derive(Serialize)]
pub struct EnvironmentUpload {
    pub url: String,
    pub expires_in: u32,
}

pub trait EnvironmentUploads: Send + Sync {
    type Error: Error + Send + Sync + 'static;
    fn presign(&self, project: ProjectId) -> Result<EnvironmentUpload, Self::Error>;
}
