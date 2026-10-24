use super::project::ProjectId;
use serde::{Deserialize, Serialize};
use std::error::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub struct BuildRequest {
    pub project_id: ProjectId,
    pub revision: u64,
}

pub trait BuildRequests: Send + Sync {
    type Error: Error + Send + Sync + 'static;
    fn request(&self, project: ProjectId) -> impl Future<Output = Result<(), Self::Error>> + Send;
}
