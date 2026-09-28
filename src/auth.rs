pub(crate) use crate::auth::discord::AuthSettingProvider;
use crate::encrypted_cookie::{EncryptedCookieJar, EncryptedCookieKeyProvider};
use crate::project::{ProjectManager, ProjectName};
use axum::Router;
use axum_extra::extract::cookie::Cookie;
use oauth2::AsyncHttpClient;
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;
use time::Duration;

mod discord;

#[derive(Serialize, Deserialize)]
pub struct AuthState {
    #[serde(rename = "uid")]
    pub user_id: SmolStr,
}

pub fn route<S>(_state: S) -> Router<S>
where
    for<'a> S: 'static + Clone + Send + Sync + EncryptedCookieKeyProvider + ProjectManager + AuthSettingProvider + AsyncHttpClient<'a>,
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

#[allow(unused)]
fn get_auth_state(cookie_jar: &EncryptedCookieJar, project: &ProjectName) -> Option<AuthState> {
    let cookie = cookie_jar.get(&auth_state_cookie_name(project))?;
    let auth_state: AuthState = serde_json::from_str(cookie.value()).ok()?;
    Some(auth_state)
}
