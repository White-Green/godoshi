use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{Request, Response};
use axum::middleware::Next;
use axum::response::IntoResponse;
use oauth2::http::StatusCode;
use serde::{Deserialize, Deserializer, Serialize};
use smol_str::SmolStr;
use std::fmt::{Debug, Display, Formatter};

#[derive(Clone, Serialize)]
#[serde(transparent)]
pub struct ProjectName(SmolStr);

impl ProjectName {
    pub fn new(name: SmolStr) -> Option<ProjectName> {
        name.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            .then_some(ProjectName(name))
    }
}

impl<'de> Deserialize<'de> for ProjectName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let name = SmolStr::deserialize(deserializer)?;
        ProjectName::new(name).ok_or_else(|| serde::de::Error::custom("invalid project name"))
    }
}

impl Debug for ProjectName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Debug::fmt(&self.0, f)
    }
}

impl Display for ProjectName {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        Display::fmt(&self.0, f)
    }
}

impl AsRef<str> for ProjectName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl PartialEq<str> for ProjectName {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

pub trait ProjectManager {
    fn project_exists(&self, project: &ProjectName) -> bool;
}

#[derive(Debug, Deserialize)]
pub struct ProjectNamePath {
    project: ProjectName,
}

#[worker::send]
pub async fn middleware<S>(
    State(state): State<S>,
    Path(ProjectNamePath { project }): Path<ProjectNamePath>,
    mut request: Request<Body>,
    next: Next,
) -> Response<Body>
where
    S: ProjectManager,
{
    if !state.project_exists(&project) {
        tracing::info!(%project, "Project not found");
        return StatusCode::NOT_FOUND.into_response();
    }
    tracing::debug!(%project, "Project resolved");
    request.extensions_mut().insert(project);
    next.run(request).await
}
