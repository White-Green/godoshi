use crate::auth::set_auth_state;
use crate::config::GenericConfigProvider;
use crate::encrypted_cookie::{EncryptedCookieJar, EncryptedCookieKeyProvider};
use crate::http::{HttpClient, OAuthHttpClient};
use crate::project::ProjectName;
use crate::user;
use crate::user::UserId;
use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{Response, StatusCode};
use axum::response::{IntoResponse, Redirect};
use axum::{Router, http};
use axum_extra::extract::cookie::Cookie;
use oauth2::basic::{
    BasicClient, BasicErrorResponse, BasicErrorResponseType, BasicRevocationErrorResponse, BasicTokenIntrospectionResponse, BasicTokenResponse,
    BasicTokenType,
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
    S: 'static + Clone + Send + Sync + EncryptedCookieKeyProvider + AuthSettingProvider + GenericConfigProvider + HttpClient,
{
    Router::new()
        .route("/", axum::routing::get(auth::<S>))
        .route("/callback", axum::routing::get(callback::<S>))
}

fn auth_client<P: AuthSettingProvider + GenericConfigProvider>(
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
        .set_redirect_uri(RedirectUrl::new(format!("{}{AUTH_FLOW_STATE_COOKIE_PATH}", provider.base_url().trim_end_matches('/'))).unwrap())
}

const AUTH_FLOW_STATE_COOKIE_NAME: &str = "godoshi_auth_flow_state";
const AUTH_FLOW_STATE_COOKIE_PATH: &str = "/auth/discord/callback";

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

fn project_redirect(project: &ProjectName, redirect: &str) -> Option<SmolStr> {
    if project.as_ref().is_empty() || redirect.chars().any(|c| c.is_ascii_control()) {
        return None;
    }
    let path = redirect.split(['?', '#']).next()?;
    let suffix = path.strip_prefix('/')?.strip_prefix(project.as_ref())?;
    // Browsers recognize percent-encoded dots in parent path segments too.
    let path = path.to_ascii_lowercase().replace("%2e", ".");
    ((suffix.is_empty() || suffix.starts_with('/')) && !path.contains("..")).then(|| redirect.into())
}

#[tracing::instrument(name = "discord_auth", skip_all, fields(%project))]
async fn auth<S>(State(state): State<S>, cookie_jar: EncryptedCookieJar, Query(AuthParam { project, redirect }): Query<AuthParam>) -> Response<Body>
where
    S: EncryptedCookieKeyProvider + AuthSettingProvider + GenericConfigProvider,
{
    let Some(redirect) = project_redirect(&project, &redirect) else {
        tracing::warn!(reason = "invalid_redirect", "Discord login rejected");
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
            .path(AUTH_FLOW_STATE_COOKIE_PATH)
            .max_age(time::Duration::seconds(600))
            .build(),
    );

    tracing::info!("Discord login started");
    (cookie_jar, Redirect::to(auth_url.as_str())).into_response()
}

#[derive(Debug, Deserialize)]
struct CallbackParam {
    code: AuthorizationCode,
    state: CsrfToken,
}

#[worker::send]
#[tracing::instrument(name = "discord_callback", skip_all, fields(project))]
async fn callback<S>(
    State(state): State<S>,
    cookie_jar: EncryptedCookieJar,
    Query(CallbackParam {
        code: exchange_code,
        state: csrf_token_from_callback,
    }): Query<CallbackParam>,
) -> Response<Body>
where
    S: EncryptedCookieKeyProvider + AuthSettingProvider + GenericConfigProvider + HttpClient,
{
    let auth_flow_state_cookie = cookie_jar.get(AUTH_FLOW_STATE_COOKIE_NAME);
    let cookie_jar = cookie_jar.remove(Cookie::build(AUTH_FLOW_STATE_COOKIE_NAME).path(AUTH_FLOW_STATE_COOKIE_PATH));
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
        tracing::warn!(reason = "missing_or_invalid_flow_cookie", "Discord callback rejected");
        return (cookie_jar, StatusCode::BAD_REQUEST).into_response();
    };
    tracing::Span::current().record("project", tracing::field::display(&project));
    if csrf_token_from_callback != csrf_token {
        tracing::warn!(reason = "csrf_mismatch", "Discord callback rejected");
        return (cookie_jar, StatusCode::BAD_REQUEST).into_response();
    }
    let token = match auth_client(&state)
        .exchange_code(exchange_code)
        .set_pkce_verifier(pkce_verifier)
        .request_async(&OAuthHttpClient(&state))
        .await
    {
        Ok(response) => response,
        Err(error) => {
            tracing::warn!(reason = token_error_reason(&error), "Discord authorization code exchange failed");
            return (cookie_jar, StatusCode::BAD_REQUEST).into_response();
        }
    };

    let access_token = token.access_token();
    assert_eq!(token.token_type(), &BasicTokenType::Bearer);

    let auth_state = match make_internal_user(&state, access_token).await {
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

    tracing::info!("Discord login succeeded");
    (cookie_jar, Redirect::to(&redirect)).into_response()
}

fn token_error_reason<E: Error + 'static>(error: &RequestTokenError<E, BasicErrorResponse>) -> &'static str {
    match error {
        RequestTokenError::ServerResponse(response) => match response.error() {
            BasicErrorResponseType::InvalidClient => "invalid_client",
            BasicErrorResponseType::InvalidGrant => "invalid_grant",
            BasicErrorResponseType::InvalidRequest => "invalid_request",
            BasicErrorResponseType::InvalidScope => "invalid_scope",
            BasicErrorResponseType::UnauthorizedClient => "unauthorized_client",
            BasicErrorResponseType::UnsupportedGrantType => "unsupported_grant_type",
            BasicErrorResponseType::Extension(_) => "provider_error",
        },
        RequestTokenError::Request(_) => "http_error",
        RequestTokenError::Parse(..) => "invalid_response",
        RequestTokenError::Other(_) => "unexpected_response",
    }
}

#[derive(Debug, Error)]
enum ConstructAuthStateError<E> {
    #[error(transparent)]
    HttpError(E),
    #[error(transparent)]
    DeserializeError(#[from] serde_json::Error),
    #[error("Bot account")]
    BotAccount,
    #[error("User not found")]
    UserNotFound,
}

#[tracing::instrument(name = "discord_profile", skip_all)]
async fn make_internal_user<S>(state: &S, access_token: &AccessToken) -> Result<UserId, ConstructAuthStateError<S::Error>>
where
    S: GenericConfigProvider + HttpClient,
{
    let request = http::request::Builder::new()
        .uri("https://discord.com/api/users/@me")
        .header(http::header::AUTHORIZATION, format!("Bearer {}", access_token.secret()))
        .body(Vec::new())
        .unwrap();

    let response = state
        .call(request)
        .await
        .inspect_err(|_| tracing::warn!(reason = "http_error", "Discord profile lookup failed"))
        .map_err(ConstructAuthStateError::HttpError)?;

    tracing::info!("{}", String::from_utf8_lossy(response.body().as_slice()));

    let me_response = serde_json::from_slice::<MeResponse>(response.body().as_slice()).inspect_err(|error| {
        tracing::warn!(
            status = response.status().as_u16(),
            reason = "invalid_response",
            category = ?error.classify(),
            "Discord profile lookup failed {error:?}"
        );
    })?;
    if me_response.bot {
        tracing::warn!(reason = "bot_account", "Discord profile rejected");
        return Err(ConstructAuthStateError::BotAccount);
    }

    let db = state.db();
    let Some(discord_user) = user::discord::find(&db, me_response.id, me_response.username).await else {
        return Err(ConstructAuthStateError::UserNotFound);
    };

    return Ok(discord_user.id);

    #[derive(Deserialize)]
    struct MeResponse {
        id: SmolStr,
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
    #[error("User not found")]
    UserNotFound,
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
            ConstructAuthStateError::UserNotFound => RefreshAuthStateError::UserNotFound,
        }
    }
}

pub type TryRefreshAuthStateResult<E> = (EncryptedCookieJar, Result<(UserId, time::Duration), RefreshAuthStateError<E>>);

#[tracing::instrument(name = "discord_refresh", skip_all, fields(%project))]
pub async fn try_refresh_auth_state<S>(state: &S, cookie_jar: EncryptedCookieJar, project: &ProjectName) -> TryRefreshAuthStateResult<S::Error>
where
    S: AuthSettingProvider + GenericConfigProvider + HttpClient,
{
    let Some(DiscordAuthState { refresh_token }) = get_discord_auth_state(&cookie_jar, project) else {
        tracing::debug!("No refresh token available");
        return (cookie_jar, Err(RefreshAuthStateError::RefreshTokenNotExists));
    };

    let token = match auth_client(state)
        .exchange_refresh_token(&refresh_token)
        .request_async(&OAuthHttpClient(state))
        .await
    {
        Ok(token) => token,
        Err(err) => {
            tracing::warn!(reason = token_error_reason(&err), "Discord token refresh failed");
            return (cookie_jar, Err(RefreshAuthStateError::TokenExchangeError(err)));
        }
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

    let auth_state = match make_internal_user(state, token.access_token()).await {
        Ok(auth_state) => auth_state,
        Err(err) => return (cookie_jar, Err(err.into())),
    };

    let expires_in = time::Duration::seconds(token.expires_in().map_or(3600, |duration| duration.as_secs() as i64));
    tracing::info!("Discord session refreshed");
    (cookie_jar, Ok((auth_state, expires_in)))
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
        .path(format!("/{project}"))
        .max_age(time::Duration::days(30))
        .build(),
    )
}

fn get_discord_auth_state(cookie_jar: &EncryptedCookieJar, project: &ProjectName) -> Option<DiscordAuthState> {
    let cookie = cookie_jar.get(&format!("{DISCORD_AUTH_STATE_COOKIE_NAME}_{project}"))?;
    let discord_auth_state = serde_json::from_str::<DiscordAuthState>(cookie.value())
        .inspect_err(|_| tracing::warn!(%project, "Invalid Discord refresh cookie"))
        .ok()?;
    Some(discord_auth_state)
}
