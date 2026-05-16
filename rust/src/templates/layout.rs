use maud::{html, Markup, PreEscaped, DOCTYPE};

use crate::models::User;

/// Common rendering context every page needs. Owned so handlers can build
/// it in one line without lifetime juggling.
pub struct PageCtx {
    pub user: Option<User>,
    pub csrf_token: String,
    /// When true, emits <meta name="robots" content="noindex,nofollow">.
    /// Set on /me/*, /admin, /api/*, /submit, /login, /register pages.
    pub no_index: bool,
    pub flash: Option<String>,
    /// Used to highlight the current section in the topnav (e.g. "/", "/submit").
    pub current_path: String,
    /// Plaintext email-verification token, stashed in the session by the
    /// register and resend-verification handlers. Lets the verify-banner
    /// render a direct "Verify my email →" button without waiting for a
    /// delivered email — used as a fallback when the outbound mail
    /// provider can't yet send.
    /// Persists until the user successfully verifies (the /verify/{token}
    /// handler clears it on success).
    pub pending_verify_token: Option<String>,
    /// Plaintext email-change confirmation token, stashed by the
    /// /me/email POST handler so /me/edit can render an inline "Confirm
    /// new email →" button. Same inline-fallback pattern. Cleared by
    /// the /confirm-email-change/{token} handler on success.
    pub pending_email_change_token: Option<String>,
    /// Unread-notification count for the logged-in user. 0 for
    /// anonymous viewers. Drives the bell badge in the topbar.
    pub unread_notifications: i64,
    /// Optional OpenGraph + Twitter-card metadata for this page.
    /// Populated by routes that produce a sharable resource (mostly
    /// /abs/{id}). Left None on internal-only pages.
    pub og: Option<OgMeta>,
    /// Optional schema.org JSON-LD blob, already serialised to a
    /// string. Routes set this for indexable content (manuscript pages
    /// with ScholarlyArticle).
    pub jsonld: Option<String>,
    /// Canonical absolute URL for the page (`<link rel="canonical">`).
    /// Helps search engines pick one URL when multiple resolve to the
    /// same content (e.g. id vs slug, with/without trailing slash).
    pub canonical_url: Option<String>,
}

/// Compact view of OpenGraph metadata. Just enough for a sharable card
/// on Twitter / X / Bluesky / Mastodon / iMessage previews. We hold
/// owned strings so the route can build the values dynamically.
#[derive(Debug, Clone)]
pub struct OgMeta {
    pub title: String,
    pub description: String,
    pub url: String,
    pub kind: &'static str, // "article" / "website" / "profile"
    pub published_time: Option<String>,
    pub modified_time: Option<String>,
    pub author: Option<String>,
}

const BRAND_SVG: &str = r##"<svg viewBox="0 0 64 64" width="32" height="32" aria-hidden="true"><rect width="64" height="64" rx="12" fill="#fff"/><path d="M 14 14 L 50 50" stroke="#b8430a" stroke-width="8" stroke-linecap="round"/><path d="M 50 14 L 14 50" stroke="#b8430a" stroke-width="3.5" stroke-linecap="round"/><circle cx="32" cy="32" r="2.6" fill="#fff"/></svg>"##;

/// Cache-buster appended as a `?v=` query string on every reference to
/// our own CSS and JS. Bump on any deploy that ships a stylesheet or
/// script change so the browser re-fetches instead of replaying its
/// stale copy. (Bump format: yyyymmdd-letter — increments alphabetically
/// for same-day re-deploys.)
const ASSET_VER: &str = "20260516c";

fn nav_class(current: &str, target: &str) -> &'static str {
    if current == target {
        "on"
    } else {
        ""
    }
}

pub fn layout(title: &str, ctx: &PageCtx, body: Markup) -> Markup {
    let cur = ctx.current_path.as_str();
    // OG/Twitter description falls back to the static site tagline if
    // the route didn't supply one, so generic pages still preview
    // reasonably when shared.
    let default_desc =
        "PreXiv: a research manuscript archive with AI-use provenance, hosted artifacts, version history, and optional human audit statements.";
    let og_title = ctx.og.as_ref().map(|o| o.title.as_str()).unwrap_or(title);
    let og_desc = ctx
        .og
        .as_ref()
        .map(|o| o.description.as_str())
        .unwrap_or(default_desc);
    let og_url = ctx
        .og
        .as_ref()
        .map(|o| o.url.as_str())
        .or(ctx.canonical_url.as_deref())
        .unwrap_or("");
    let og_type = ctx.og.as_ref().map(|o| o.kind).unwrap_or("website");
    html! {
        (DOCTYPE)
        html lang="en" data-theme="auto" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width,initial-scale=1";
                title { (title) " · PreXiv" }
                meta name="description" content=(og_desc);
                @if ctx.no_index {
                    meta name="robots" content="noindex,nofollow";
                }
                @if let Some(canonical) = &ctx.canonical_url {
                    link rel="canonical" href=(canonical);
                }

                // ── OpenGraph (LinkedIn, Facebook, iMessage, Slack, Discord) ──
                meta property="og:site_name" content="PreXiv";
                meta property="og:locale"    content="en_US";
                meta property="og:type"      content=(og_type);
                meta property="og:title"     content=(og_title);
                meta property="og:description" content=(og_desc);
                @if !og_url.is_empty() {
                    meta property="og:url"   content=(og_url);
                }
                @if let Some(og) = &ctx.og {
                    @if let Some(pt) = &og.published_time {
                        meta property="article:published_time" content=(pt);
                    }
                    @if let Some(mt) = &og.modified_time {
                        meta property="article:modified_time"  content=(mt);
                    }
                    @if let Some(au) = &og.author {
                        meta property="article:author" content=(au);
                    }
                }

                // ── Twitter / X card ─────────────────────────────────────────
                meta name="twitter:card"        content="summary";
                meta name="twitter:title"       content=(og_title);
                meta name="twitter:description" content=(og_desc);

                // ── schema.org JSON-LD (Google Scholar, indexers) ────────────
                @if let Some(jsonld) = &ctx.jsonld {
                    script type="application/ld+json" { (PreEscaped(jsonld.as_str())) }
                }

                link rel="stylesheet" href={ "/static/vendor/fonts/cormorant/cormorant-garamond.css?v=" (ASSET_VER) };
                link rel="stylesheet" href={ "/static/css/style.css?v=" (ASSET_VER) };
                link rel="stylesheet" href={ "/static/css/prexiv-rust.css?v=" (ASSET_VER) };
                link rel="icon" type="image/svg+xml" href="/static/favicon.svg";

                // KaTeX — render $…$ / $$…$$ / \(…\) / \[…\] math in
                // abstracts, comments, and conductor notes.
                link rel="stylesheet" href={ "/static/vendor/katex/katex.min.css?v=" (ASSET_VER) };
                script defer src={ "/static/vendor/katex/katex.min.js?v=" (ASSET_VER) } {}
                script defer src={ "/static/vendor/katex/contrib/auto-render.min.js?v=" (ASSET_VER) } {}
                script defer src={ "/static/js/katex-init.js?v=" (ASSET_VER) } {}
                script defer src={ "/static/js/copy-button.js?v=" (ASSET_VER) } {}
                script defer src={ "/static/js/prexiv-ui.js?v=" (ASSET_VER) } {}
                @if cur == "/" {
                    script defer src={ "/static/js/welcome-modal.js?v=" (ASSET_VER) } {}
                }
            }
            body {
                a.skip-link href="#main-content" { "Skip to main content" }
                header.topbar {
                    div.topbar-inner {
                        a.brand href="/" aria-label="PreXiv home" {
                            span.brand-mark { (PreEscaped(BRAND_SVG)) }
                            span.brand-name {
                                span.bp { "Pre" }
                                span.bx { "X" }
                                span.bi { "iv" }
                            }
                            span.brand-tagline { "AI-use provenance archive" }
                        }
                        nav.topnav aria-label="Main navigation" {
                            a href="/"        class=(nav_class(cur, "/"))        { "ranked" }
                            a href="/new"     class=(nav_class(cur, "/new"))     { "new" }
                            a href="/top"     class=(nav_class(cur, "/top"))     { "top" }
                            a href="/audited" class=(nav_class(cur, "/audited")) { "audited" }
                            a href="/browse"  class=(if cur.starts_with("/browse") { "on" } else { "" }) { "browse" }
                            @if ctx.user.is_some() {
                                a href="/feed" class=(nav_class(cur, "/feed")) { "feed" }
                            }
                            a href="/submit"  class=(if cur == "/submit" { "on submit-link" } else { "submit-link" }) { "submit" }
                            a href="/about"   class=(nav_class(cur, "/about")) { "about" }
                            @if let Some(u) = &ctx.user {
                                @if u.is_admin() {
                                    a href="/admin" class=(if cur == "/admin" { "on admin-link" } else { "admin-link" }) { "admin" }
                                }
                            }
                        }
                        form.searchbox action="/search" method="get" role="search" {
                            label.visually-hidden for="topbar-search" { "Search manuscripts" }
                            input id="topbar-search" type="search" name="q" placeholder="search title, author, id…";
                        }
                        div.userbox {
                            @if let Some(u) = &ctx.user {
                                a.me href={ "/u/" (u.username) } { (u.username) }
                                span.karma title="karma" { "(" (u.karma.unwrap_or(0)) ")" }
                                span.sep { "·" }
                                a.notif-link href="/me/notifications" title="Notifications" {
                                    "🔔"
                                    @if ctx.unread_notifications > 0 {
                                        span.notif-badge { (ctx.unread_notifications) }
                                    }
                                }
                                span.sep { "·" }
                                a href="/me/tokens" title="manage your API tokens" { "API tokens" }
                                span.sep { "·" }
                                form.logout-form action="/logout" method="post" {
                                    input type="hidden" name="csrf_token" value=(ctx.csrf_token);
                                    button type="submit" { "logout" }
                                }
                            } @else {
                                a href="/login" { "login" }
                                span.sep { "·" }
                                a href="/register" { "register" }
                            }
                        }
                    }
                }
                @if let Some(msg) = &ctx.flash {
                    div.flash role="status" { (msg) }
                }
                main.container id="main-content" { (body) }
                footer.sitefooter {
                    div.footer-inner {
                        nav.footer-nav aria-label="Site links" {
                            a.footer-brand-text href="/" {
                                span.bp { "Pre" }
                                span.bx { "X" }
                                span.bi { "iv" }
                            }
                            span.footer-sep aria-hidden="true" { "·" }
                            a href="/about"      { "About" }
                            span.footer-sep aria-hidden="true" { "·" }
                            a href="/how-it-works" { "How it works" }
                            span.footer-sep aria-hidden="true" { "·" }
                            a href="/agent-support" { "Agent support" }
                            span.footer-sep aria-hidden="true" { "·" }
                            a href="/guidelines" { "Guidelines" }
                            span.footer-sep aria-hidden="true" { "·" }
                            a href="/submit"     { "Submit" }
                            span.footer-spacer aria-hidden="true" {}
                            a href="/tos"        { "ToS" }
                            span.footer-sep aria-hidden="true" { "·" }
                            a href="/privacy"    { "Privacy" }
                            span.footer-sep aria-hidden="true" { "·" }
                            a href="/licenses"   { "Licenses" }
                            span.footer-sep aria-hidden="true" { "·" }
                            a href="/permissions" { "Permissions" }
                            span.footer-sep aria-hidden="true" { "·" }
                            a href="/dmca"       { "DMCA" }
                            span.footer-sep aria-hidden="true" { "·" }
                            a href="/policies"   { "Policies" }
                        }
                        p.footer-meta {
                            "© " (chrono::Utc::now().format("%Y")) " PreXiv. Research manuscripts with AI-use provenance. Manuscripts here have not undergone formal peer review."
                        }
                    }
                }
            }
        }
    }
}

/// Render an external link with the appropriate rel attributes for user-
/// submitted content (so we don't pass page-rank to spam links).
pub fn external_link(url: &str, label: &str) -> Markup {
    html! {
        a href=(url) rel="nofollow ugc noopener" target="_blank" { (label) }
    }
}

/// Best-effort "N minutes/hours/days ago" string for a SQL DATETIME.
pub fn time_ago(ts: &chrono::NaiveDateTime) -> String {
    let now = chrono::Utc::now().naive_utc();
    let dur = now.signed_duration_since(*ts);
    let secs = dur.num_seconds().max(0);
    if secs < 60 {
        return format!("{secs}s ago");
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}m ago");
    }
    let hours = mins / 60;
    if hours < 24 {
        return format!("{hours}h ago");
    }
    let days = hours / 24;
    if days < 30 {
        return format!("{days}d ago");
    }
    let months = days / 30;
    if months < 12 {
        return format!("{months}mo ago");
    }
    let years = days / 365;
    format!("{years}y ago")
}
