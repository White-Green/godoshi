use crate::HOST;
use crate::auth::{AuthState, set_auth_state};
use crate::encrypted_cookie::{EncryptedCookieJar, EncryptedCookieKeyProvider};
use crate::http::{HttpClient, OAuthHttpClient};
use crate::project::{ProjectManager, ProjectName};
use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{Response, StatusCode};
use axum::response::{IntoResponse, Redirect};
use axum::{Router, http};
use axum_extra::extract::cookie::Cookie;
use oauth2::basic::{
    BasicClient, BasicErrorResponse, BasicRevocationErrorResponse, BasicTokenIntrospectionResponse, BasicTokenResponse, BasicTokenType,
};
use oauth2::{
    AccessToken, AuthUrl, AuthorizationCode, Client, ClientId, ClientSecret, CsrfToken, EndpointNotSet, EndpointSet, PkceCodeChallenge,
    PkceCodeVerifier, RedirectUrl, RefreshToken, RequestTokenError, RevocationUrl, Scope, StandardRevocableToken, TokenResponse, TokenUrl,
};
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;
use std::error::Error;
use thiserror::Error;

pub trait AuthSettingProvider {
    fn client_id(&self) -> String;
    fn client_secret(&self) -> String;
}

pub fn route<S>() -> Router<S>
where
    S: 'static + Clone + Send + Sync + EncryptedCookieKeyProvider + ProjectManager + AuthSettingProvider + HttpClient,
{
    Router::new()
        .route("/", axum::routing::get(auth::<S>))
        .route("/callback", axum::routing::get(callback::<S>))
}

fn auth_client<P: AuthSettingProvider>(
    provider: &P,
) -> Client<
    BasicErrorResponse,
    BasicTokenResponse,
    BasicTokenIntrospectionResponse,
    StandardRevocableToken,
    BasicRevocationErrorResponse,
    EndpointSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointSet,
    EndpointSet,
> {
    const AUTH_URL: &str = "https://discord.com/oauth2/authorize";
    const TOKEN_URL: &str = "https://discord.com/api/oauth2/token";
    const REVOKE_URL: &str = "https://discord.com/api/oauth2/token/revoke";

    BasicClient::new(ClientId::new(provider.client_id()))
        .set_client_secret(ClientSecret::new(provider.client_secret()))
        .set_auth_uri(AuthUrl::new(AUTH_URL.to_string()).unwrap())
        .set_token_uri(TokenUrl::new(TOKEN_URL.to_string()).unwrap())
        .set_revocation_url(RevocationUrl::new(REVOKE_URL.to_string()).unwrap())
        .set_redirect_uri(RedirectUrl::new(format!("https://{HOST}/auth/discord/callback")).unwrap())
}

const AUTH_FLOW_STATE_COOKIE_NAME: &str = "godoshi_auth_flow_state";

#[derive(Debug, Serialize, Deserialize)]
struct AuthFlowState {
    #[serde(rename = "pid")]
    project: ProjectName,
    #[serde(rename = "rd")]
    redirect: SmolStr,
    #[serde(rename = "pkce")]
    pkce_verifier: PkceCodeVerifier,
    #[serde(rename = "csrf")]
    csrf_token: CsrfToken,
}

#[derive(Debug, Deserialize)]
struct AuthParam {
    project: ProjectName,
    redirect: SmolStr,
}

async fn auth<S>(State(state): State<S>, cookie_jar: EncryptedCookieJar, Query(AuthParam { project, redirect }): Query<AuthParam>) -> Response<Body>
where
    S: EncryptedCookieKeyProvider + ProjectManager + AuthSettingProvider,
{
    if let Some(suffix) = redirect.strip_prefix('/')
        && let Some(suffix) = suffix.strip_prefix(project.as_ref())
        && (suffix.is_empty() || suffix.starts_with('/'))
        && state.project_exists(&project)
    {
    } else {
        return StatusCode::BAD_REQUEST.into_response();
    };

    let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();
    let (auth_url, csrf_token) = auth_client(&state)
        .authorize_url(CsrfToken::new_random)
        .add_scope(Scope::new("identify".to_string()))
        .set_pkce_challenge(pkce_challenge)
        .url();

    let auth_flow_state = AuthFlowState {
        project,
        redirect,
        pkce_verifier,
        csrf_token,
    };

    let cookie_jar = cookie_jar.add(
        Cookie::build((AUTH_FLOW_STATE_COOKIE_NAME, serde_json::to_string(&auth_flow_state).unwrap()))
            .http_only(true)
            .secure(true)
            .path("/auth/discord/callback")
            .max_age(time::Duration::seconds(600))
            .build(),
    );

    (cookie_jar, Redirect::to(auth_url.as_str())).into_response()
}

#[derive(Debug, Deserialize)]
struct CallbackParam {
    code: AuthorizationCode,
    state: CsrfToken,
}

#[worker::send]
async fn callback<S>(
    State(state): State<S>,
    cookie_jar: EncryptedCookieJar,
    Query(CallbackParam {
        code: exchange_code,
        state: csrf_token_from_callback,
    }): Query<CallbackParam>,
) -> Response<Body>
where
    S: EncryptedCookieKeyProvider + AuthSettingProvider + HttpClient,
{
    let auth_flow_state_cookie = cookie_jar.get(AUTH_FLOW_STATE_COOKIE_NAME);
    let cookie_jar = cookie_jar.add(Cookie::build(AUTH_FLOW_STATE_COOKIE_NAME).max_age(time::Duration::seconds(0)));
    let AuthFlowState {
        project,
        redirect,
        pkce_verifier,
        csrf_token,
    } = if let Some(auth_flow_state) = auth_flow_state_cookie
        && let Ok(auth_flow_state) = serde_json::from_str(auth_flow_state.value())
    {
        auth_flow_state
    } else {
        return (cookie_jar, StatusCode::BAD_REQUEST).into_response();
    };
    if csrf_token_from_callback != csrf_token {
        return (cookie_jar, StatusCode::BAD_REQUEST).into_response();
    }
    let token = match auth_client(&state)
        .exchange_code(exchange_code)
        .set_pkce_verifier(pkce_verifier)
        .request_async(&OAuthHttpClient(&state))
        .await
    {
        Ok(response) => response,
        Err(_) => {
            return (cookie_jar, StatusCode::BAD_REQUEST).into_response();
        }
    };

    let access_token = token.access_token();
    assert_eq!(token.token_type(), &BasicTokenType::Bearer);

    let auth_state = match construct_auth_state(&state, access_token).await {
        Ok(auth_state) => auth_state,
        Err(_) => return (cookie_jar, StatusCode::BAD_REQUEST).into_response(),
    };

    let expires_in = time::Duration::seconds(token.expires_in().map_or(3600, |duration| duration.as_secs() as i64));
    let cookie_jar = set_auth_state(cookie_jar, &project, auth_state, expires_in);
    let cookie_jar = if let Some(refresh_token) = token.refresh_token() {
        set_discord_auth_state(
            cookie_jar,
            &project,
            DiscordAuthState {
                refresh_token: refresh_token.clone(),
            },
        )
    } else {
        cookie_jar
    };

    (cookie_jar, Redirect::to(&redirect)).into_response()
}

#[derive(Debug, Error)]
enum ConstructAuthStateError<E> {
    #[error(transparent)]
    HttpError(E),
    #[error(transparent)]
    DeserializeError(#[from] serde_json::Error),
    #[error("Bot account")]
    BotAccount,
}

async fn construct_auth_state<S>(state: &S, access_token: &AccessToken) -> Result<AuthState, ConstructAuthStateError<S::Error>>
where
    S: HttpClient,
{
    let request = http::request::Builder::new()
        .uri("https://discord.com/api/users/@me")
        .header(http::header::AUTHORIZATION, format!("Bearer {}", access_token.secret()))
        .body(Vec::new())
        .unwrap();

    let response = state.call(request).await.map_err(ConstructAuthStateError::HttpError)?;

    let me_response = serde_json::from_slice::<MeResponse>(response.body().as_slice())?;
    if me_response.bot {
        return Err(ConstructAuthStateError::BotAccount);
    }

    return Ok(AuthState {
        user_id: me_response.username,
    });

    #[derive(Deserialize)]
    struct MeResponse {
        username: SmolStr,
        #[serde(default)]
        bot: bool,
    }
}

#[derive(Debug, Error)]
pub enum RefreshAuthStateError<E: Error + 'static> {
    #[error(transparent)]
    HttpError(E),
    #[error(transparent)]
    DeserializeError(#[from] serde_json::Error),
    #[error("Bot account")]
    BotAccount,
    #[error("Refresh token not exists")]
    RefreshTokenNotExists,
    #[error(transparent)]
    TokenExchangeError(#[from] RequestTokenError<E, BasicErrorResponse>),
}

impl<E: Error + 'static> From<ConstructAuthStateError<E>> for RefreshAuthStateError<E> {
    fn from(value: ConstructAuthStateError<E>) -> Self {
        match value {
            ConstructAuthStateError::HttpError(e) => RefreshAuthStateError::HttpError(e),
            ConstructAuthStateError::DeserializeError(e) => RefreshAuthStateError::DeserializeError(e),
            ConstructAuthStateError::BotAccount => RefreshAuthStateError::BotAccount,
        }
    }
}

pub async fn try_refresh_auth_state<S>(
    state: &S,
    cookie_jar: EncryptedCookieJar,
    project: &ProjectName,
) -> (EncryptedCookieJar, Result<AuthState, RefreshAuthStateError<S::Error>>)
where
    S: AuthSettingProvider + HttpClient,
{
    let Some(DiscordAuthState { refresh_token }) = get_discord_auth_state(&cookie_jar, project) else {
        return (cookie_jar, Err(RefreshAuthStateError::RefreshTokenNotExists));
    };

    let token = match auth_client(state)
        .exchange_refresh_token(&refresh_token)
        .request_async(&OAuthHttpClient(state))
        .await
    {
        Ok(token) => token,
        Err(err) => return (cookie_jar, Err(RefreshAuthStateError::TokenExchangeError(err))),
    };

    let cookie_jar = if let Some(refresh_token) = token.refresh_token() {
        set_discord_auth_state(
            cookie_jar,
            project,
            DiscordAuthState {
                refresh_token: refresh_token.clone(),
            },
        )
    } else {
        cookie_jar
    };

    let auth_state = match construct_auth_state(state, token.access_token()).await {
        Ok(auth_state) => auth_state,
        Err(err) => return (cookie_jar, Err(err.into())),
    };

    (cookie_jar, Ok(auth_state))
}

const DISCORD_AUTH_STATE_COOKIE_NAME: &str = "godoshi_discord_auth_state";

#[derive(Debug, Serialize, Deserialize)]
struct DiscordAuthState {
    refresh_token: RefreshToken,
}

fn set_discord_auth_state(cookie_jar: EncryptedCookieJar, project: &ProjectName, discord_auth_state: DiscordAuthState) -> EncryptedCookieJar {
    cookie_jar.add(
        Cookie::build((
            format!("{DISCORD_AUTH_STATE_COOKIE_NAME}_{project}"),
            serde_json::to_string(&discord_auth_state).unwrap(),
        ))
        .http_only(true)
        .secure(true)
        .max_age(time::Duration::days(30))
        .build(),
    )
}

fn get_discord_auth_state(cookie_jar: &EncryptedCookieJar, project: &ProjectName) -> Option<DiscordAuthState> {
    let cookie = cookie_jar.get(&format!("{DISCORD_AUTH_STATE_COOKIE_NAME}_{project}"))?;
    let discord_auth_state = serde_json::from_str::<DiscordAuthState>(cookie.value()).ok()?;
    Some(discord_auth_state)
}
