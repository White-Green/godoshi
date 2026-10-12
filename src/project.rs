use crate::config::GenericConfigProvider;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{Request, Response};
use axum::middleware::Next;
use axum::response::IntoResponse;
use oauth2::http::StatusCode;
use serde::{Deserialize, Deserializer, Serialize};
use smol_str::SmolStr;
use std::fmt;
use std::fmt::{Debug, Display};
use worker::D1Database;

#[cfg(feature = "worker-tests")]
pub mod tests;

#[derive(Clone, PartialEq, Eq, Serialize)]
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
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Debug::fmt(&self.0, f)
    }
}

impl Display for ProjectName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProjectId(u64);

impl Display for ProjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Display::fmt(&self.0, f)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectState {
    pub id: ProjectId,
    pub name: ProjectName,
}

async fn find(db: &D1Database, name: &ProjectName) -> worker::Result<Option<ProjectId>> {
    let query = worker::query!(db, "select id from project where name = ?", name.as_ref()).expect("Project query parameters must be valid D1 values");
    let results = query
        .all()
        .await?
        .results::<QueryResult>()
        .expect("Project query results must match the schema");
    return Ok(match results.as_slice() {
        [] => None,
        [result] => Some(result.id),
        _ => unreachable!("Multiple projects with the same name"),
    });

    #[derive(Deserialize)]
    struct QueryResult {
        id: ProjectId,
    }
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
    S: GenericConfigProvider,
{
    let id = match find(&state.db(), &project).await {
        Ok(id) => id,
        Err(e) => {
            tracing::error!(%project, error = %e, "Project database lookup failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let Some(id) = id else {
        tracing::info!(%project, "Project not found");
        return StatusCode::NOT_FOUND.into_response();
    };
    tracing::debug!(%project, "Project resolved");
    request.extensions_mut().insert(ProjectState { id, name: project });
    next.run(request).await
}
