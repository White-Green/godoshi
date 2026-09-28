use crate::encrypted_cookie::{EncryptedCookieJar, EncryptedCookieKeyProvider};
use crate::http::HttpClient;
use crate::project::{ProjectManager, ProjectName};
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{Request, Response};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect};
use axum::{Extension, Router};
use axum_extra::extract::cookie::Cookie;
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;
use time::Duration;

pub mod discord;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthState {
    #[serde(rename = "uid")]
    pub user_id: SmolStr,
}

pub fn route<S>() -> Router<S>
where
    S: 'static + Clone + Send + Sync + EncryptedCookieKeyProvider + ProjectManager + discord::AuthSettingProvider + HttpClient,
{
    Router::new().nest("/discord", discord::route::<S>())
}

const AUTH_STATE_COOKIE_NAME: &str = "godoshi_auth_state";

fn auth_state_cookie_name(project: &ProjectName) -> String {
    format!("{AUTH_STATE_COOKIE_NAME}_{project}")
}

fn set_auth_state(cookie_jar: EncryptedCookieJar, project: &ProjectName, auth_state: AuthState, age: Duration) -> EncryptedCookieJar {
    cookie_jar.add(
        Cookie::build((auth_state_cookie_name(project), serde_json::to_string(&auth_state).unwrap()))
            .path(format!("/{}", project))
            .max_age(age)
            .build(),
    )
}

fn get_auth_state(cookie_jar: &EncryptedCookieJar, project: &ProjectName) -> Option<AuthState> {
    let cookie = cookie_jar.get(&auth_state_cookie_name(project))?;
    let auth_state: AuthState = serde_json::from_str(cookie.value()).ok()?;
    Some(auth_state)
}

#[worker::send]
pub async fn middleware<S>(
    State(state): State<S>,
    Path(path): Path<String>,
    mut cookie_jar: EncryptedCookieJar,
    Extension(project): Extension<ProjectName>,
    mut request: Request<Body>,
    next: Next,
) -> Response<Body>
where
    S: EncryptedCookieKeyProvider + discord::AuthSettingProvider + HttpClient,
{
    if let Some(auth_state) = get_auth_state(&cookie_jar, &project) {
        request.extensions_mut().insert(auth_state);
    } else {
        let (new_cookie_jar, result) = discord::try_refresh_auth_state(&state, cookie_jar, &project).await;
        cookie_jar = new_cookie_jar;

        match result {
            Ok(auth_state) => {
                request.extensions_mut().insert(auth_state);
            }
            Err(_) => {
                return (cookie_jar, Redirect::to(&format!("/auth/discord?project={project}&redirect={path}"))).into_response();
            }
        }
    }

    let response = next.run(request).await;
    (cookie_jar, response).into_response()
}
