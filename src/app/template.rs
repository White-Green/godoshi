use askama::Template;

/// ```askama
/// <!DOCTYPE html>
/// <html lang="ja">
/// <head>
///     <meta charset="utf-8" />
///     <meta name="viewport" content="width=device-width, initial-scale=1" />
///     <title>{{ title }}</title>
///     <link rel="stylesheet" href="/static/bootstrap.min.css" />
/// </head>
/// <body class="bg-body-tertiary">
/// <a class="visually-hidden-focusable d-block bg-white p-2" href="#main">本文へ移動</a>
/// <header class="text-bg-dark py-3"><div class="container"><span class="fs-5 fw-bold">Godoshi</span><span class="ms-3 small">合同誌制作</span></div></header>
/// <main id="main" class="container py-4 py-md-5">{{ inner|safe }}</main>
/// </body>
/// </html>
/// ```
#[derive(Debug, Template)]
#[template(ext = "html", in_doc = true)]
pub struct HtmlTemplate<'a, T: Template> {
    title: &'a str,
    inner: T,
}

impl<T: Template> HtmlTemplate<'_, T> {
    pub fn new(title: &str, inner: T) -> HtmlTemplate<'_, T> {
        HtmlTemplate { title, inner }
    }
}
