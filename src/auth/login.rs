use askama::Template;
use axum::body::Body;
use axum::http::{Method, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};

/// ```askama
/// <!DOCTYPE html>
/// <html lang="ja">
/// <head>
///     <meta charset="utf-8" />
///     <meta name="viewport" content="width=device-width, initial-scale=1" />
///     <title>ログイン | Godoshi</title>
///     <meta property="og:title" content="Godoshi" />
///     <meta property="og:description" content="Godoshi" />
///     <meta property="og:type" content="website" />
///     <meta property="og:site_name" content="Godoshi" />
///     <meta property="og:url" content="{{ page_url }}" />
///     <link rel="stylesheet" href="/static/bootstrap.min.css" />
/// </head>
/// <body class="bg-body-tertiary">
/// <header class="text-bg-dark py-3"><div class="container"><span class="fs-5 fw-bold">Godoshi</span></div></header>
/// <main class="container py-4">
///     <h1 class="h2 fw-bold mb-4">ログイン</h1>
///     <p>ログイン画面へ移動します。自動で移動しない場合は、下のボタンを押してください。</p>
///     <a id="login-link" href="{{ login_url }}" class="btn btn-primary">ログインする</a>
/// </main>
/// <script>
///     window.location.replace(document.getElementById("login-link").href);
/// </script>
/// </body>
/// </html>
/// ```
#[derive(Template)]
#[template(ext = "html", in_doc = true)]
struct LoginTemplate<'a> {
    login_url: &'a str,
    page_url: &'a str,
}

pub(super) fn response(login_url: &str, page_url: &str, method: &Method) -> Response {
    let mut response = if matches!(*method, Method::GET | Method::HEAD) {
        Html(LoginTemplate { login_url, page_url }.render().expect("Login template must render")).into_response()
    } else {
        (StatusCode::UNAUTHORIZED, "ログインし直してから操作してください。").into_response()
    };
    response.headers_mut().insert(header::CACHE_CONTROL, "private, no-store".parse().unwrap());
    if *method == Method::HEAD {
        *response.body_mut() = Body::empty();
    }
    response
}
