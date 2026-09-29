use crate::config::BaseUrlProvider;
use crate::encrypted_cookie::{EncryptedCookieJar, EncryptedCookieKeyProvider};
use crate::http::HttpClient;
use crate::project::ProjectName;
use axum::body::Body;
use axum::extract::{OriginalUri, State};
use axum::http::{Request, Response};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect};
use axum::{Extension, Router};
use axum_extra::extract::cookie::Cookie;
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;
use time::{Duration, OffsetDateTime};

pub mod discord;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthState {
    #[serde(rename = "uid")]
    pub user_id: SmolStr,
    pub username: SmolStr,
}

#[derive(Serialize, Deserialize)]
struct AuthStateCookie {
    #[serde(flatten)]
    auth_state: AuthState,
    #[serde(rename = "exp")]
    expires_at: i64,
}

pub fn route<S>() -> Router<S>
where
    S: 'static + Clone + Send + Sync + EncryptedCookieKeyProvider + discord::AuthSettingProvider + BaseUrlProvider + HttpClient,
{
    Router::new().nest("/discord", discord::route::<S>())
}

const AUTH_STATE_COOKIE_NAME: &str = "godoshi_auth_state";

fn auth_state_cookie_name(project: &ProjectName) -> String {
    format!("{AUTH_STATE_COOKIE_NAME}_{project}")
}

fn set_auth_state(cookie_jar: EncryptedCookieJar, project: &ProjectName, auth_state: AuthState, age: Duration) -> EncryptedCookieJar {
    let auth_state = AuthStateCookie {
        auth_state,
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

fn get_auth_state(cookie_jar: &EncryptedCookieJar, project: &ProjectName) -> Option<AuthState> {
    let cookie = cookie_jar.get(&auth_state_cookie_name(project))?;
    let auth_state: AuthStateCookie = serde_json::from_str(cookie.value())
        .inspect_err(|_| tracing::warn!(%project, "Invalid session cookie"))
        .ok()?;
    if auth_state.expires_at <= OffsetDateTime::now_utc().unix_timestamp() {
        tracing::debug!(%project, "Session expired");
        return None;
    }
    Some(auth_state.auth_state)
}

#[worker::send]
pub async fn middleware<S>(
    State(state): State<S>,
    OriginalUri(uri): OriginalUri,
    mut cookie_jar: EncryptedCookieJar,
    Extension(project): Extension<ProjectName>,
    mut request: Request<Body>,
    next: Next,
) -> Response<Body>
where
    S: EncryptedCookieKeyProvider + discord::AuthSettingProvider + BaseUrlProvider + HttpClient,
{
    if let Some(auth_state) = get_auth_state(&cookie_jar, &project) {
        tracing::debug!(%project, "Authenticated using session cookie");
        request.extensions_mut().insert(auth_state);
    } else {
        let (new_cookie_jar, result) = discord::try_refresh_auth_state(&state, cookie_jar, &project).await;

        cookie_jar = match result {
            Ok((auth_state, expires_in)) => {
                request.extensions_mut().insert(auth_state.clone());
                set_auth_state(new_cookie_jar, &project, auth_state, expires_in)
            }
            Err(_) => {
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
