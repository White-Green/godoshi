use crate::config::GenericConfigProvider;
use crate::encrypted_cookie::{EncryptedCookieJar, EncryptedCookieKeyProvider};
use crate::http::HttpClient;
use crate::project::{ProjectName, ProjectState};
use crate::user::{UserId, UserPermissionSet};
use axum::body::Body;
use axum::extract::{OriginalUri, State};
use axum::http::{Request, Response, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect};
use axum::{Extension, Router};
use axum_extra::extract::cookie::Cookie;
use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime};

pub mod discord;

#[cfg(feature = "worker-tests")]
pub mod tests;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthState {
    pub user_id: UserId,
    pub project: ProjectState,
    pub permission_set: UserPermissionSet,
}

#[derive(Serialize, Deserialize)]
struct AuthStateCookie {
    id: UserId,
    #[serde(rename = "exp")]
    expires_at: i64,
}

pub fn route<S>() -> Router<S>
where
    S: 'static + Clone + Send + Sync + EncryptedCookieKeyProvider + discord::AuthSettingProvider + GenericConfigProvider + HttpClient,
{
    Router::new().nest("/discord", discord::route::<S>())
}

const AUTH_STATE_COOKIE_NAME: &str = "godoshi_auth_state";

fn auth_state_cookie_name(project: &ProjectName) -> String {
    format!("{AUTH_STATE_COOKIE_NAME}_{project}")
}

fn set_auth_state(cookie_jar: EncryptedCookieJar, project: &ProjectName, authenticated_user_id: UserId, age: Duration) -> EncryptedCookieJar {
    let auth_state = AuthStateCookie {
        id: authenticated_user_id,
        expires_at: OffsetDateTime::now_utc().unix_timestamp().saturating_add(age.whole_seconds()),
    };
    cookie_jar.add(
        Cookie::build((auth_state_cookie_name(project), serde_json::to_string(&auth_state).unwrap()))
            .http_only(true)
            .secure(true)
            .path(format!("/{}", project))
            .max_age(age)
            .build(),
    )
}

fn get_authenticated_user_id(cookie_jar: &EncryptedCookieJar, project: &ProjectName) -> Option<UserId> {
    let cookie = cookie_jar.get(&auth_state_cookie_name(project))?;
    let auth_state: AuthStateCookie = serde_json::from_str(cookie.value())
        .inspect_err(|_| tracing::warn!(%project, "Invalid session cookie"))
        .ok()?;
    if auth_state.expires_at <= OffsetDateTime::now_utc().unix_timestamp() {
        tracing::debug!(%project, "Session expired");
        return None;
    }
    Some(auth_state.id)
}

async fn construct_auth_state<S>(state: &S, user_id: UserId, project: ProjectState) -> Result<AuthState, StatusCode>
where
    S: GenericConfigProvider,
{
    let db = state.db();
    let query = worker::query!(
        &db,
        "select permission_set from permission where project_id = ? and user_id = ?",
        project.id,
        user_id
    )
    .expect("Permission query parameters must be valid D1 values");
    let result = query
        .all()
        .await
        .map_err(|_| {
            tracing::error!("User permissions database lookup failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .results::<QueryResult>()
        .expect("Permission query results must match the schema and permission bits");

    let permission_set = match result.as_slice() {
        [] => UserPermissionSet::empty(),
        &[QueryResult { permission_set }] => permission_set,
        _ => unreachable!(),
    };

    return Ok(AuthState {
        user_id,
        project,
        permission_set,
    });

    #[derive(Debug, Deserialize)]
    struct QueryResult {
        permission_set: UserPermissionSet,
    }
}

#[worker::send]
pub async fn middleware<S>(
    State(state): State<S>,
    OriginalUri(uri): OriginalUri,
    mut cookie_jar: EncryptedCookieJar,
    Extension(project_state): Extension<ProjectState>,
    mut request: Request<Body>,
    next: Next,
) -> Response<Body>
where
    S: EncryptedCookieKeyProvider + discord::AuthSettingProvider + GenericConfigProvider + HttpClient,
{
    let project = project_state.name.clone();
    if let Some(user_id) = get_authenticated_user_id(&cookie_jar, &project) {
        tracing::debug!(%project, "Authenticated using session cookie");
        let auth_state = match construct_auth_state(&state, user_id, project_state).await {
            Ok(auth_state) => auth_state,
            Err(status) => return (cookie_jar, status).into_response(),
        };
        request.extensions_mut().insert(auth_state);
    } else {
        let (new_cookie_jar, result) = discord::try_refresh_auth_state(&state, cookie_jar, &project).await;

        cookie_jar = match result {
            Ok((user_id, expires_in)) => {
                let auth_state = match construct_auth_state(&state, user_id, project_state).await {
                    Ok(auth_state) => auth_state,
                    Err(status) => return (new_cookie_jar, status).into_response(),
                };
                request.extensions_mut().insert(auth_state);
                set_auth_state(new_cookie_jar, &project, user_id, expires_in)
            }
            Err(error) => {
                if let Some(status) = error.status_code() {
                    return (new_cookie_jar, status).into_response();
                }
                tracing::debug!(%project, "Redirecting unauthenticated request to Discord login");
                let redirect = uri.path_and_query().map_or(uri.path(), |path| path.as_str());
                let query = oauth2::url::form_urlencoded::Serializer::new(String::new())
                    .append_pair("project", project.as_ref())
                    .append_pair("redirect", redirect)
                    .finish();
                return (new_cookie_jar, Redirect::to(&format!("/auth/discord?{query}"))).into_response();
            }
        }
    }

    let response = next.run(request).await;
    (cookie_jar, response).into_response()
}
