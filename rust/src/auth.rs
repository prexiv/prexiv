//! Password hashing, HIBP k-anonymity check, session+user extraction.
//!
//! Password hashes use bcrypt cost 10 so they're cross-compatible with the
//! JS app's bcryptjs hashes — a user registered through either app can log
//! in via the other.

use std::time::Duration;

use crate::db::DbPool;
use axum::extract::{FromRef, FromRequestParts};
use axum::http::request::Parts;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::Json;
use serde_json::json;
use sha1::{Digest, Sha1};
use tower_sessions::Session;

use crate::api_auth::{bearer_token_in_query, extract_bearer, find_user_by_bearer, BearerToken};
use crate::models::User;
use crate::state::AppState;

const SESSION_USER_KEY: &str = "user_id";
const SESSION_CSRF_KEY: &str = "csrf_token";

pub fn hash_password(plain: &str) -> Result<String, bcrypt::BcryptError> {
    bcrypt::hash(plain, 10)
}

/// A real bcrypt hash of a value no user will ever submit. Used by
/// `verify_password_timing_safe` to keep the no-such-user branch
/// indistinguishable (by wall-clock) from the wrong-password branch.
///
/// Cost 10 to match `hash_password`. Generated once with bcrypt cost 10
/// of a random 32-byte value the user can never produce.
const DUMMY_BCRYPT_HASH: &str = "$2b$10$zLMZvyd5qzLz7CV9OX1KF.VTSvKmiD.x3i2yTTC3Cw1VdKB5Qfzoy";

/// Always run bcrypt on `plain`. If the user was found we use their real
/// hash; if not, we use a dummy hash so the no-such-user branch costs the
/// same wall-clock time as the wrong-password branch. Always returns
/// `false` in the dummy case. This closes a classic user-enumeration
/// timing oracle on /login.
pub fn verify_password_timing_safe(plain: &str, real_hash: Option<&str>) -> bool {
    match real_hash {
        Some(h) => bcrypt::verify(plain, h).unwrap_or(false),
        None => {
            let _ = bcrypt::verify(plain, DUMMY_BCRYPT_HASH);
            false
        }
    }
}

/// HIBP k-anonymity check: send only the first 5 SHA-1 hex chars to the
/// pwnedpasswords range API and scan for our suffix. On any error or
/// timeout, warn-and-allow (return false) — never block a registration on
/// a network blip.
pub async fn is_password_pwned(password: &str) -> bool {
    if password.is_empty() {
        return false;
    }
    let mut hasher = Sha1::new();
    hasher.update(password.as_bytes());
    let digest = hasher.finalize();
    let hex = hex::encode_upper(digest);
    let (prefix, suffix) = hex.split_at(5);

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .user_agent("PreXiv-pwned-check")
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };

    let url = format!("https://api.pwnedpasswords.com/range/{prefix}");
    let res = match client.get(&url).header("Add-Padding", "true").send().await {
        Ok(r) if r.status().is_success() => r,
        _ => return false,
    };
    let body = match res.text().await {
        Ok(t) => t,
        Err(_) => return false,
    };
    body.lines().any(|line| {
        line.split(':')
            .next()
            .map(|hex| hex.eq_ignore_ascii_case(suffix))
            .unwrap_or(false)
    })
}

pub async fn current_user_id(session: &Session) -> Option<i64> {
    session.get::<i64>(SESSION_USER_KEY).await.ok().flatten()
}

/// Establish a logged-in session for `user_id`.
///
/// We rotate the session id (`cycle_id`) before writing the user id, to
/// defend against **session fixation**: if an attacker can plant a known
/// cookie value in the victim's browser (via shared device, XSS that sets
/// document.cookie, etc.) and the victim then logs in, without `cycle_id`
/// the attacker keeps an authenticated handle on that fixed cookie. With
/// `cycle_id`, login mints a fresh server-side session id, so the
/// pre-login cookie becomes worthless. Standard OWASP guidance.
pub async fn login_session(session: &Session, user_id: i64) -> anyhow::Result<()> {
    session.cycle_id().await?;
    session.insert(SESSION_USER_KEY, user_id).await?;
    Ok(())
}

pub async fn logout_session(session: &Session) -> anyhow::Result<()> {
    session.flush().await?;
    Ok(())
}

/// Get-or-generate the CSRF token for this session. Stable across requests
/// in the same session so the form-field check works on POST.
pub async fn csrf_token(session: &Session) -> String {
    if let Ok(Some(t)) = session.get::<String>(SESSION_CSRF_KEY).await {
        return t;
    }
    use rand::RngCore;
    let mut bytes = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut bytes);
    let token = hex::encode(bytes);
    let _ = session.insert(SESSION_CSRF_KEY, token.clone()).await;
    token
}

pub async fn verify_csrf(session: &Session, submitted: &str) -> bool {
    match session.get::<String>(SESSION_CSRF_KEY).await.ok().flatten() {
        Some(expected) => constant_time_eq(expected.as_bytes(), submitted.as_bytes()),
        None => false,
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

pub async fn load_user(pool: &DbPool, user_id: i64) -> Result<Option<User>, sqlx::Error> {
    let mut u = sqlx::query_as::<_, User>(crate::db::pg(
        r#"SELECT id, username, email, display_name, affiliation, bio,
                  karma, is_admin, email_verified, orcid, created_at,
                  email_enc, orcid_verified, institutional_email,
                  orcid_oauth_verified, orcid_oauth_verified_at, orcid_oauth_sub,
                  github_oauth_verified, github_oauth_verified_at, github_id, github_login
           FROM users WHERE id = ?"#,
    ))
    .bind(user_id)
    .fetch_optional(pool)
    .await?;
    if let Some(u) = u.as_mut() {
        u.resolve_email();
    }
    Ok(u)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthSource {
    Session,
    Bearer,
}

fn bearer_auth_error(message: &str) -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({
            "error": message,
            "hint": "send `Authorization: Bearer prexiv_…` — mint a token at /me/tokens"
        })),
    )
        .into_response()
}

fn bearer_url_error() -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({
            "error": "bearer tokens are not accepted in URLs",
            "hint": "send `Authorization: Bearer prexiv_…`; never put an API token in a query string"
        })),
    )
        .into_response()
}

/// Optional current user — extracted on every request. Use when the page
/// renders differently for logged-in vs anonymous (most pages).
pub struct MaybeUser(pub Option<User>);

impl<S> FromRequestParts<S> for MaybeUser
where
    AppState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = Response;
    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let app: AppState = AppState::from_ref(state);
        if let BearerToken::Present(plain) = extract_bearer(parts) {
            if let Some(user) = find_user_by_bearer(&app.pool, &plain).await {
                return Ok(MaybeUser(Some(user)));
            }
        }

        let session = match Session::from_request_parts(parts, state).await {
            Ok(s) => s,
            Err(_) => return Ok(MaybeUser(None)),
        };
        let uid = current_user_id(&session).await;
        let user = match uid {
            Some(id) => load_user(&app.pool, id).await.ok().flatten(),
            None => None,
        };
        Ok(MaybeUser(user))
    }
}

/// Required current user plus the credential source that authenticated the
/// request. Bearer-token auth lets agents exercise browser-form routes
/// without a cookie session; CSRF checks can then stay session-only and be
/// explicitly bypassed only for token-authenticated requests.
pub struct RequireAuthUser {
    pub user: User,
    pub source: AuthSource,
}

impl<S> FromRequestParts<S> for RequireAuthUser
where
    AppState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = Response;
    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let app: AppState = AppState::from_ref(state);
        match extract_bearer(parts) {
            BearerToken::Present(plain) => {
                return match find_user_by_bearer(&app.pool, &plain).await {
                    Some(user) => Ok(RequireAuthUser {
                        user,
                        source: AuthSource::Bearer,
                    }),
                    None => Err(bearer_auth_error("invalid or expired bearer token")),
                };
            }
            BearerToken::Malformed => {
                return Err(bearer_auth_error(
                    "missing or malformed Authorization header",
                ));
            }
            BearerToken::Missing if bearer_token_in_query(parts) => return Err(bearer_url_error()),
            BearerToken::Missing => {}
        }

        let session = match Session::from_request_parts(parts, state).await {
            Ok(s) => s,
            Err(_) => {
                let path = parts
                    .uri
                    .path_and_query()
                    .map(|p| p.as_str())
                    .unwrap_or("/");
                let target = format!("/login?next={}", urlencoding::encode(path));
                return Err(Redirect::to(&target).into_response());
            }
        };
        let user = match current_user_id(&session).await {
            Some(id) => load_user(&app.pool, id).await.ok().flatten(),
            None => None,
        };
        match user {
            Some(user) => Ok(RequireAuthUser {
                user,
                source: AuthSource::Session,
            }),
            None => {
                let path = parts
                    .uri
                    .path_and_query()
                    .map(|p| p.as_str())
                    .unwrap_or("/");
                let target = format!("/login?next={}", urlencoding::encode(path));
                Err(Redirect::to(&target).into_response())
            }
        }
    }
}

/// Required current user — redirects to /login if absent. Use on private
/// pages (/submit, /me/*, /admin, etc.).
pub struct RequireUser(pub User);

impl<S> FromRequestParts<S> for RequireUser
where
    AppState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = Response;
    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let auth = RequireAuthUser::from_request_parts(parts, state).await?;
        Ok(RequireUser(auth.user))
    }
}

/// Admin-only — 403 if not admin.
pub struct RequireAdmin(pub User);

impl<S> FromRequestParts<S> for RequireAdmin
where
    AppState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = Response;
    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let RequireUser(user) = RequireUser::from_request_parts(parts, state).await?;
        if user.is_admin() {
            Ok(RequireAdmin(user))
        } else {
            Err((StatusCode::FORBIDDEN, "Admin only").into_response())
        }
    }
}
