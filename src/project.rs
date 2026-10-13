use crate::core::Config;
use crate::core::project::{ProjectName, ProjectStore};
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{Request, Response, StatusCode};
use axum::middleware::Next;
use axum::response::IntoResponse;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct ProjectNamePath {
    project: ProjectName,
}

pub async fn middleware<S>(
    State(state): State<S>,
    Path(ProjectNamePath { project }): Path<ProjectNamePath>,
    mut request: Request<Body>,
    next: Next,
) -> Response<Body>
where
    S: Config,
{
    let found = match state.projects().find_project(project.as_ref()).await {
        Ok(id) => id,
        Err(e) => {
            tracing::error!(%project, error = %e, "Project database lookup failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let Some(found) = found else {
        tracing::info!(%project, "Project not found");
        return StatusCode::NOT_FOUND.into_response();
    };
    tracing::debug!(%project, "Project resolved");
    request.extensions_mut().insert(found);
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::project::Project;
    use crate::test_support::TestConfig;
    use crate::test_support::project::{self, TestProject};
    use axum::{Extension, Router};
    use tower_service::Service;

    #[test]
    fn middleware_resolves_only_the_requested_project() {
        futures::executor::block_on(async {
            let config = TestConfig::default();
            project::insert(&config.db(), TestProject { id: 1, name: "alpha".into() }).await;
            let mut router = Router::new()
                .route(
                    "/{project}/",
                    axum::routing::get(async |Extension(project): Extension<Project>| project.name.to_string()),
                )
                .layer(axum::middleware::from_fn_with_state(config, middleware::<TestConfig>));
            let response = router.call(Request::builder().uri("/alpha/").body(Body::empty()).unwrap()).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(axum::body::to_bytes(response.into_body(), 1024).await.unwrap(), "alpha");
            for name in ["beta", "Alpha", "alph"] {
                let response = router
                    .call(Request::builder().uri(format!("/{name}/")).body(Body::empty()).unwrap())
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::NOT_FOUND);
            }
        });
    }
}
