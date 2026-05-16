use axum::http::header;
use axum::response::IntoResponse;

const FAVICON_SVG: &str = include_str!("../../../public/favicon.svg");

const ROBOTS_TXT: &str = "User-agent: *
Disallow: /admin
Disallow: /admin/
Disallow: /me
Disallow: /me/
Disallow: /api
Disallow: /api/
Disallow: /login
Disallow: /register
Disallow: /logout
Disallow: /submit
Disallow: /vote
Allow: /
Allow: /abs/
Allow: /pdf/
Allow: /src/
Allow: /search
Allow: /u/
Allow: /static/

Sitemap: /sitemap.xml
";

#[allow(dead_code)]
const _: &str = "Robots policy: /admin and /me are private; /api is for agents not crawlers; /sitemap.xml is the canonical index.";

pub async fn robots_txt() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        ROBOTS_TXT,
    )
}

pub async fn favicon() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "image/svg+xml")], FAVICON_SVG)
}
