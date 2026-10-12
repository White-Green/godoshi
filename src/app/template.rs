use askama::Template;

/// ```askama
/// <!DOCTYPE html>
/// <html>
/// <head>
///     <meta charset="utf-8" />
///     <title>{{ title }}</title>
/// </head>
/// <body>
/// {{ inner|safe }}
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
