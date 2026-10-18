use crate::core::Config;
use crate::core::project::Project;
use crate::core::user::{ProjectMember, ProjectUserStore, UserId, UserStore};
use crate::encrypted_cookie::{EncryptedCookieJar, EncryptedCookieKeyProvider};
use crate::http::HttpClient;
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthState {
    pub project: Project,
    pub user: ProjectMember,
}

#[derive(Serialize, Deserialize)]
struct AuthStateCookie {
    id: UserId,
    #[serde(rename = "exp")]
    expires_at: i64,
}

pub fn route<S>() -> Router<S>
where
    S: 'static + Clone + Send + Sync + EncryptedCookieKeyProvider + discord::AuthSettingProvider + Config + HttpClient,
{
    Router::new().nest("/discord", discord::route::<S>())
}

const AUTH_STATE_COOKIE_NAME: &str = "godoshi_auth_state";

fn auth_state_cookie_name(project: &Project) -> String {
    format!("{AUTH_STATE_COOKIE_NAME}_{}", project.name())
}

fn set_auth_state(cookie_jar: EncryptedCookieJar, project: &Project, authenticated_user_id: UserId, age: Duration) -> EncryptedCookieJar {
    let auth_state = AuthStateCookie {
        id: authenticated_user_id,
        expires_at: OffsetDateTime::now_utc().unix_timestamp().saturating_add(age.whole_seconds()),
    };
    cookie_jar.add(
        Cookie::build((auth_state_cookie_name(project), serde_json::to_string(&auth_state).unwrap()))
            .http_only(true)
            .secure(true)
            .path(format!("/{}", project.name()))
            .max_age(age)
            .build(),
    )
}

fn get_authenticated_user_id(cookie_jar: &EncryptedCookieJar, project: &Project) -> Option<UserId> {
    let cookie = cookie_jar.get(&auth_state_cookie_name(project))?;
    let auth_state: AuthStateCookie = serde_json::from_str(cookie.value())
        .inspect_err(|_| tracing::warn!(?project, "Invalid session cookie"))
        .ok()?;
    if auth_state.expires_at <= OffsetDateTime::now_utc().unix_timestamp() {
        tracing::debug!(?project, "Session expired");
        return None;
    }
    Some(auth_state.id)
}

async fn construct_auth_state<S>(state: &S, user_id: UserId, project: Project) -> Result<AuthState, StatusCode>
where
    S: Config,
{
    let user = state
        .users()
        .project(project.id)
        .find_member(user_id)
        .await
        .map_err(|error| {
            tracing::error!(%error, "Project member lookup failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::FORBIDDEN)?;
    Ok(AuthState { project, user })
}

pub async fn middleware<S>(
    State(state): State<S>,
    OriginalUri(uri): OriginalUri,
    mut cookie_jar: EncryptedCookieJar,
    Extension(project): Extension<Project>,
    mut request: Request<Body>,
    next: Next,
) -> Response<Body>
where
    S: EncryptedCookieKeyProvider + discord::AuthSettingProvider + Config + HttpClient,
{
    if let Some(user_id) = get_authenticated_user_id(&cookie_jar, &project) {
        tracing::debug!(?project, "Authenticated using session cookie");
        let auth_state = match construct_auth_state(&state, user_id, project).await {
            Ok(auth_state) => auth_state,
            Err(status) => return (cookie_jar, status).into_response(),
        };
        request.extensions_mut().insert(auth_state);
    } else {
        let (new_cookie_jar, result) = discord::try_refresh_auth_state(&state, cookie_jar, &project).await;

        cookie_jar = match result {
            Ok((user_id, expires_in)) => {
                let auth_state = match construct_auth_state(&state, user_id, project.clone()).await {
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
                tracing::debug!(?project, "Redirecting unauthenticated request to Discord login");
                let redirect = uri.path_and_query().map_or(uri.path(), |path| path.as_str());
                let query = oauth2::url::form_urlencoded::Serializer::new(String::new())
                    .append_pair("project", project.name().as_ref())
                    .append_pair("redirect", redirect)
                    .finish();
                return (new_cookie_jar, Redirect::to(&format!("/auth/discord?{query}"))).into_response();
            }
        }
    }

    let response = next.run(request).await;
    (cookie_jar, response).into_response()
}
