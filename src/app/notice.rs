use crate::project::ProjectState;
use axum::Extension;
use axum::extract::{OriginalUri, Request};
use axum::http::{Uri, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum_extra::extract::CookieJar;
use axum_extra::extract::cookie::{Cookie, SameSite};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use time::Duration;

const COOKIE_NAME: &str = "godoshi_notice";

/// Request extensions contain the incoming notices. Response extensions contain
/// notices for the next request, scoped to the redirect destination or current path.
/// The cookie uses a fixed project Path; its value holds the exact destination path.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub(super) struct Notices(pub BTreeMap<String, Value>);

impl<K: Into<String>, V: Into<Value>, const N: usize> From<[(K, V); N]> for Notices {
    fn from(values: [(K, V); N]) -> Self {
        Self(values.into_iter().map(|(key, value)| (key.into(), value.into())).collect())
    }
}

#[derive(Serialize, Deserialize)]
struct NoticeCookie {
    path: String,
    notices: Notices,
}

pub(super) async fn middleware(
    Extension(project): Extension<ProjectState>,
    OriginalUri(uri): OriginalUri,
    mut cookie_jar: CookieJar,
    mut request: Request,
    next: Next,
) -> Response {
    let cookie_path = format!("/{}", project.name);
    let mut notices = Notices::default();
    if let Some(cookie) = cookie_jar.get(COOKIE_NAME) {
        match serde_json::from_str::<NoticeCookie>(cookie.value()) {
            Ok(cookie) if cookie.path == uri.path() => {
                notices = cookie.notices;
                cookie_jar = cookie_jar.remove(Cookie::build(COOKIE_NAME).path(cookie_path.clone()));
            }
            Ok(_) => {}
            Err(_) => {
                cookie_jar = cookie_jar.remove(Cookie::build(COOKIE_NAME).path(cookie_path.clone()));
            }
        }
    }
    request.extensions_mut().insert(notices);

    let mut response = next.run(request).await;

    let outgoing = response.extensions_mut().remove::<Notices>();
    let Some(notices) = outgoing.filter(|notices| !notices.0.is_empty()) else {
        return (cookie_jar, response).into_response();
    };
    let path = response
        .headers()
        .get(header::LOCATION)
        .and_then(|location| location.to_str().ok())
        .and_then(|location| location.parse::<Uri>().ok())
        .map_or_else(|| uri.path().to_owned(), |location| location.path().to_owned());
    let value = serde_json::to_string(&NoticeCookie { path, notices }).expect("Notices must serialize as JSON");
    let cookie_jar = cookie_jar.add(
        Cookie::build((COOKIE_NAME, value))
            .path(cookie_path)
            .http_only(true)
            .secure(true)
            .same_site(SameSite::Lax)
            .max_age(Duration::minutes(5))
            .build(),
    );
    (cookie_jar, response).into_response()
}
