use crate::auth::AuthState;
use askama::Template;
use smallvec::SmallVec;
use smol_str::{SmolStr, format_smolstr};

#[derive(Debug)]
struct Breadcrumb<'a> {
    label: &'a str,
    href: Option<SmolStr>,
}

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
/// <header class="text-bg-dark py-3"><div class="container d-flex align-items-baseline gap-3"><span class="fs-5 fw-bold">Godoshi</span><span class="small text-break">{{ project_name }}</span></div></header>
/// <main id="main" class="container py-4">
/// {% if !breadcrumbs.is_empty() %}
/// <nav aria-label="パンくずリスト" style="--bs-breadcrumb-divider: '&gt;';">
/// <ol class="breadcrumb mb-3">
/// {% for item in breadcrumbs %}
///     {% if let Some(href) = item.href %}<li class="breadcrumb-item"><a href="{{ href }}">{{ item.label }}</a></li>
///     {% else %}<li class="breadcrumb-item active" aria-current="page">{{ item.label }}</li>{% endif %}
/// {% endfor %}
/// </ol>
/// </nav>
/// {% endif %}
/// {{ inner|safe }}
/// </main>
/// </body>
/// </html>
/// ```
#[derive(Debug, Template)]
#[template(ext = "html", in_doc = true)]
pub struct HtmlTemplate<'a, T: Template> {
    title: &'a str,
    project_name: &'a str,
    breadcrumbs: SmallVec<[Breadcrumb<'a>; 3]>,
    inner: T,
}

impl<'a, T: Template> HtmlTemplate<'a, T> {
    /// Pairs of (title, path relative to the project root), from root to current page.
    pub fn new(auth: &'a AuthState, pages: &[(&'a str, &'a str)], inner: T) -> Self {
        let project_name = auth.project.name.as_ref();
        let mut breadcrumbs = SmallVec::<[Breadcrumb<'a>; 3]>::new();
        if pages.len() > 1 {
            for (index, &(label, path)) in pages.iter().enumerate() {
                if index == 0 && auth.user.permissions().bits().count_ones() < 2 {
                    continue;
                }
                breadcrumbs.push(Breadcrumb {
                    label,
                    href: (index < pages.len() - 1).then(|| format_smolstr!("/{project_name}/{path}")),
                });
            }
        }
        HtmlTemplate {
            title: if pages.len() <= 1 { project_name } else { pages.last().unwrap().0 },
            project_name,
            breadcrumbs,
            inner,
        }
    }
}
