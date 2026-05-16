use std::net::SocketAddr;

use std::sync::Arc;

use anyhow::Context;
use axum::http::{header, HeaderValue};
use axum::Router;
use time::Duration;
use tower_governor::governor::GovernorConfigBuilder;
use tower_governor::GovernorLayer;
use tower_http::compression::CompressionLayer;
use tower_http::services::ServeDir;
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::TraceLayer;
use tower_sessions::cookie::SameSite;
use tower_sessions::{Expiry, SessionManagerLayer};
use tower_sessions_sqlx_store::PostgresStore;
use tracing_subscriber::EnvFilter;

mod api_auth;
mod auth;
mod categories;
mod compile;
mod crockford;
mod crypto;
mod db;
mod email;
mod email_change;
mod error;
mod github;
mod helpers;
mod licenses;
mod markdown;
mod models;
mod notifications;
mod orcid;
mod passwords;
mod pdf_watermark;
mod routes;
mod state;
mod templates;
mod totp;
mod verify;
mod versions;

use crate::state::AppState;

type InstitutionalEmailBackfillRow = (i64, Option<String>, Option<Vec<u8>>, i64);

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,sqlx=warn,tower_http=debug"));
    let structured_logs = std::env::var("PREXIV_LOG_FORMAT")
        .map(|value| value.eq_ignore_ascii_case("json"))
        .unwrap_or(false);
    if structured_logs {
        tracing_subscriber::fmt()
            .json()
            .with_env_filter(env_filter)
            .init();
    } else {
        tracing_subscriber::fmt().with_env_filter(env_filter).init();
    }

    let database_url =
        std::env::var("DATABASE_URL").context("DATABASE_URL must point to PostgreSQL")?;

    let pool = db::connect(&database_url)
        .await
        .context("connecting to PostgreSQL")?;

    sqlx::migrate!("./pg_migrations")
        .run(&pool)
        .await
        .context("running sqlx migrations")?;

    // S-7: load the email-at-rest master key, then backfill any legacy
    // rows still missing an encrypted email. Both steps are idempotent —
    // re-running on every startup is fine (and is the recovery path if
    // someone restores from an older backup).
    crypto::init().context("initialising PREXIV_DATA_KEY (S-7)")?;
    let backfilled = backfill_user_emails(&pool)
        .await
        .context("backfilling user email_enc / email_hash")?;
    if backfilled > 0 {
        tracing::info!("S-7 backfill: encrypted {backfilled} legacy email rows");
    }
    let inst_set = backfill_institutional_email(&pool)
        .await
        .context("backfilling users.institutional_email")?;
    if inst_set > 0 {
        tracing::info!(
            "identity-signal backfill: tagged {inst_set} users with institutional_email=1"
        );
    }
    let totp_migrated = backfill_legacy_totp(&pool)
        .await
        .context("backfilling legacy users.totp_secret into user_totp")?;
    if totp_migrated > 0 {
        tracing::info!("S-7 backfill: encrypted {totp_migrated} legacy TOTP secrets");
    }
    let webhook_secrets_migrated = backfill_webhook_secrets(&pool)
        .await
        .context("backfilling webhook.secret into webhook.secret_enc")?;
    if webhook_secrets_migrated > 0 {
        tracing::info!("S-7 backfill: encrypted {webhook_secrets_migrated} webhook secrets");
    }
    let scrubbed = scrub_legacy_secret_columns(&pool)
        .await
        .context("scrubbing legacy plaintext user-secret columns")?;
    if scrubbed > 0 {
        tracing::info!("S-7 scrub: removed {scrubbed} legacy plaintext user-secret values");
    }

    // Session store — shares the same DB so we don't need a second file.
    let session_store = PostgresStore::new(pool.clone());
    session_store
        .migrate()
        .await
        .context("running tower-sessions migrations")?;

    let secure_cookies = std::env::var("NODE_ENV").as_deref() == Ok("production");
    let session_layer = SessionManagerLayer::new(session_store)
        .with_secure(secure_cookies)
        .with_http_only(true)
        .with_same_site(SameSite::Lax)
        .with_name("prexiv_session")
        .with_expiry(Expiry::OnInactivity(Duration::days(30)));

    let app_url = std::env::var("APP_URL").ok();
    let state = AppState { pool, app_url };

    let static_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|p| p.join("public"))
        .unwrap_or_else(|| "./public".into());

    // UPLOAD_DIR lives outside the source tree on production (so a git
    // reset --hard can't delete user PDFs). We serve it under
    // /static/uploads/ via a second, more-specific nest_service that
    // takes precedence over the broader /static fallback. Without this
    // bridge the PDFs land on disk but 404 in the browser.
    let upload_dir: std::path::PathBuf = std::env::var("UPLOAD_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| static_dir.join("uploads"));

    // Security headers — set on every response.
    //
    //   X-Content-Type-Options: nosniff  → stop the browser from
    //     content-sniffing an uploaded PDF as HTML/JS.
    //   X-Frame-Options: DENY            → defeat clickjacking (no
    //     other site can embed PreXiv in an iframe).
    //   Referrer-Policy: strict-origin-when-cross-origin
    //                                    → don't leak full URLs (which
    //     may include manuscript ids that aren't yet public) to outbound
    //     links.
    //   Content-Security-Policy          → keep scripts/styles/images to
    //     the current app. Inline style is temporarily allowed because
    //     older templates still carry a few inline layout styles.
    //   Permissions-Policy                 → disable browser features PreXiv
    //     does not use, reducing the blast radius of a compromised page.
    //   Strict-Transport-Security        → only in production, where the
    //     Tailscale Funnel serves HTTPS. Browsers ignore HSTS sent over
    //     plaintext HTTP, but spec says don't send it — so we gate on
    //     `secure_cookies` (same flag that means "we're behind HTTPS").
    let security_headers = tower::ServiceBuilder::new()
        .layer(SetResponseHeaderLayer::overriding(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::X_FRAME_OPTIONS,
            HeaderValue::from_static("DENY"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::REFERRER_POLICY,
            HeaderValue::from_static("strict-origin-when-cross-origin"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(
                "default-src 'self'; base-uri 'self'; object-src 'none'; frame-ancestors 'none'; form-action 'self'; script-src 'self' 'unsafe-inline'; script-src-attr 'none'; style-src 'self' 'unsafe-inline'; font-src 'self' data:; img-src 'self' data: blob:; connect-src 'self'; upgrade-insecure-requests",
            ),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::HeaderName::from_static("permissions-policy"),
            HeaderValue::from_static(
                "camera=(), microphone=(), geolocation=(), payment=(), usb=(), interest-cohort=()",
            ),
        ));

    // Per-IP rate limiting via tower_governor. Default key extractor
    // uses the source IP (taking it from the socket; behind Tailscale
    // Funnel this is the inbound Funnel-relay IP, which still gives us
    // a usable token-bucket because a single client's requests share
    // the same source). Two buckets:
    //
    //   * `auth_governor` — applied to /login, /register, /forgot-password
    //     and the /login/2fa second-step. 1 request/second with a burst of
    //     5. Defends against credential-stuffing without annoying normal
    //     form use.
    //
    //   * `write_governor` — applied to /submit, /vote, comment posts,
    //     and the API write paths. 1 request/second with a burst of 30.
    //     Defends against vote-brigading and submission spam.
    let auth_governor = Arc::new(
        GovernorConfigBuilder::default()
            .per_second(1)
            .burst_size(5)
            .finish()
            .expect("auth GovernorConfig"),
    );
    let write_governor = Arc::new(
        GovernorConfigBuilder::default()
            .per_second(1)
            .burst_size(30)
            .finish()
            .expect("write GovernorConfig"),
    );
    let auth_layer = GovernorLayer::new(auth_governor);
    let write_layer = GovernorLayer::new(write_governor);

    let app = Router::new()
        .merge(routes::router())
        .merge(routes::auth_post_router().layer(auth_layer))
        .merge(routes::write_post_router().layer(write_layer))
        // The more-specific upload mount goes FIRST so axum picks it up
        // before the generic /static fallback. Versioned app assets get
        // long-lived immutable caching; uploaded manuscripts use shorter
        // caching because their paths are user-facing archive artifacts.
        .nest_service(
            "/static/uploads",
            tower::ServiceBuilder::new()
                .layer(SetResponseHeaderLayer::overriding(
                    header::CACHE_CONTROL,
                    HeaderValue::from_static("public, max-age=3600"),
                ))
                .service(ServeDir::new(upload_dir)),
        )
        .nest_service(
            "/static",
            tower::ServiceBuilder::new()
                .layer(SetResponseHeaderLayer::overriding(
                    header::CACHE_CONTROL,
                    HeaderValue::from_static("public, max-age=31536000, immutable"),
                ))
                .service(ServeDir::new(static_dir)),
        )
        // Unmatched routes — return the styled 404 page.
        .fallback(routes::not_found_fallback)
        .layer(security_headers)
        .layer(SetResponseHeaderLayer::if_not_present(
            header::STRICT_TRANSPORT_SECURITY,
            if secure_cookies {
                HeaderValue::from_static("max-age=31536000; includeSubDomains")
            } else {
                HeaderValue::from_static("max-age=0")
            },
        ))
        .layer(CompressionLayer::new())
        .layer(
            TraceLayer::new_for_http().make_span_with(|request: &axum::http::Request<_>| {
                tracing::debug_span!(
                    "http_request",
                    method = %request.method(),
                    path = %request.uri().path()
                )
            }),
        )
        .layer(session_layer)
        .with_state(state);

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3001);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("prexiv (rust) listening on http://{addr}");
    // ConnectInfo<SocketAddr> is required by tower_governor's default
    // PeerIpKeyExtractor — without it, every rate-limited request
    // 500s. `into_make_service_with_connect_info::<SocketAddr>` is
    // the standard fix.
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}

/// One-time pass that flips `institutional_email = 1` for any verified row
/// whose email is on the institutional-domain allowlist. Reads from
/// `email_enc` when available and falls back to the legacy plaintext column
/// only for rows that have not yet been encrypted.
async fn backfill_institutional_email(pool: &crate::db::DbPool) -> anyhow::Result<usize> {
    let rows: Vec<InstitutionalEmailBackfillRow> = sqlx::query_as(crate::db::pg(
        "SELECT id, email, email_enc, email_verified FROM users
          WHERE institutional_email = 0",
    ))
    .fetch_all(pool)
    .await?;
    let mut n = 0usize;
    for (id, email_opt, email_enc, verified) in rows {
        if verified == 0 {
            continue;
        }
        let email = match email_enc.as_deref() {
            Some(enc) => match crate::crypto::open_email(enc) {
                Ok(email) => email,
                Err(e) => {
                    tracing::error!(user_id = id, error = %e, "decrypt email_enc failed during institutional-email backfill");
                    email_opt.unwrap_or_default()
                }
            },
            None => email_opt.unwrap_or_default(),
        };
        if !crate::email::is_institutional(&email) {
            continue;
        }
        sqlx::query(crate::db::pg(
            "UPDATE users SET institutional_email = 1 WHERE id = ?",
        ))
        .bind(id)
        .execute(pool)
        .await?;
        n += 1;
    }
    Ok(n)
}

/// S-7 startup pass: encrypt the plaintext `email` column for any user row
/// whose `email_hash` is still NULL. Returns the number of rows updated.
/// Idempotent — running it twice does nothing the second time. Skips rows
/// with an empty `email` because the app keeps the legacy plaintext column
/// blank once `email_enc` is populated.
async fn backfill_user_emails(pool: &crate::db::DbPool) -> anyhow::Result<usize> {
    let rows: Vec<(i64, Option<String>)> = sqlx::query_as(crate::db::pg(
        "SELECT id, email FROM users WHERE email_hash IS NULL",
    ))
    .fetch_all(pool)
    .await?;
    let mut n = 0usize;
    for (id, email_opt) in rows {
        let email = email_opt.unwrap_or_default();
        if email.trim().is_empty() {
            continue;
        }
        let (hash, enc) = crypto::seal_email(&email)?;
        let hash_vec = hash.to_vec();
        sqlx::query(crate::db::pg(
            "UPDATE users SET email_hash = ?, email_enc = ? WHERE id = ?",
        ))
        .bind(&hash_vec)
        .bind(&enc)
        .bind(id)
        .execute(pool)
        .await?;
        n += 1;
    }
    Ok(n)
}

async fn backfill_legacy_totp(pool: &crate::db::DbPool) -> anyhow::Result<usize> {
    let rows: Vec<(i64, Option<String>, i64)> = sqlx::query_as(crate::db::pg(
        "SELECT id, totp_secret, totp_enabled FROM users
          WHERE totp_enabled != 0
            AND totp_secret IS NOT NULL
            AND TRIM(totp_secret) <> ''",
    ))
    .fetch_all(pool)
    .await?;
    let mut n = 0usize;
    for (user_id, secret_opt, enabled) in rows {
        let Some(secret) = secret_opt
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let already_migrated: Option<(i64,)> =
            sqlx::query_as(crate::db::pg("SELECT 1 FROM user_totp WHERE user_id = ?"))
                .bind(user_id)
                .fetch_optional(pool)
                .await?;
        if already_migrated.is_some() {
            continue;
        }
        let secret_enc = crate::crypto::encrypt_blob(secret.as_bytes())?;
        let enabled_at: Option<chrono::NaiveDateTime> = if enabled != 0 {
            Some(chrono::Utc::now().naive_utc())
        } else {
            None
        };
        sqlx::query(crate::db::pg(
            "INSERT INTO user_totp (user_id, secret, enabled_at)
             VALUES (?, ?, ?)
             ON CONFLICT(user_id) DO NOTHING",
        ))
        .bind(user_id)
        .bind(&secret_enc)
        .bind(enabled_at)
        .execute(pool)
        .await?;
        n += 1;
    }
    Ok(n)
}

async fn backfill_webhook_secrets(pool: &crate::db::DbPool) -> anyhow::Result<usize> {
    let rows: Vec<(i64, String)> = sqlx::query_as(crate::db::pg(
        "SELECT id, secret FROM webhooks
          WHERE secret_enc IS NULL
            AND secret IS NOT NULL
            AND secret <> ''",
    ))
    .fetch_all(pool)
    .await?;
    let mut n = 0usize;
    for (id, secret) in rows {
        let secret_enc = crate::crypto::encrypt_blob(secret.as_bytes())?;
        sqlx::query(crate::db::pg(
            "UPDATE webhooks SET secret_enc = ? WHERE id = ?",
        ))
        .bind(&secret_enc)
        .bind(id)
        .execute(pool)
        .await?;
        n += 1;
    }
    Ok(n)
}

async fn scrub_legacy_secret_columns(pool: &crate::db::DbPool) -> anyhow::Result<usize> {
    let mut n = 0usize;

    n += sqlx::query(crate::db::pg(
        "UPDATE users SET email = ''
          WHERE email_enc IS NOT NULL AND email IS NOT NULL AND email <> ''",
    ))
    .execute(pool)
    .await?
    .rows_affected() as usize;

    n += sqlx::query(crate::db::pg(
        "UPDATE pending_email_changes SET new_email = ''
          WHERE new_email_enc IS NOT NULL AND new_email IS NOT NULL AND new_email <> ''",
    ))
    .execute(pool)
    .await?
    .rows_affected() as usize;

    n += sqlx::query(crate::db::pg(
        "UPDATE users
            SET email_verify_token = NULL,
                email_verify_expires = NULL
          WHERE email_verify_token IS NOT NULL
             OR email_verify_expires IS NOT NULL",
    ))
    .execute(pool)
    .await?
    .rows_affected() as usize;

    n += sqlx::query(crate::db::pg(
        "UPDATE users
            SET password_reset_token = NULL,
                password_reset_expires = NULL
          WHERE password_reset_token IS NOT NULL
             OR password_reset_expires IS NOT NULL",
    ))
    .execute(pool)
    .await?
    .rows_affected() as usize;

    n += sqlx::query(crate::db::pg(
        "UPDATE users
            SET totp_secret = NULL,
                totp_enabled = 0
          WHERE totp_secret IS NOT NULL
             OR totp_enabled != 0",
    ))
    .execute(pool)
    .await?
    .rows_affected() as usize;

    n += sqlx::query(crate::db::pg(
        "UPDATE webhooks SET secret = ''
          WHERE secret_enc IS NOT NULL AND secret IS NOT NULL AND secret <> ''",
    ))
    .execute(pool)
    .await?
    .rows_affected() as usize;

    Ok(n)
}
