//! Bearer-token API authentication for AI agents.
//!
//! Token format: `prexiv_` + 36 base64url chars (27 random bytes), exactly
//! matching the JS app's `generateToken()`. Stored as SHA-256 hex in
//! `api_tokens.token_hash`; only a short display prefix is stored beside the
//! hash. The plaintext is shown to the caller exactly once at creation and
//! never persisted.
//!
//! `ApiUser` is an axum extractor that pulls the bearer from the
//! `Authorization` header, looks the hash up, honours `expires_at`, and
//! touches `last_used_at` so token-management UIs can show recency.

use crate::db::DbPool;
use axum::extract::{FromRef, FromRequestParts};
use axum::http::header;
use axum::http::request::Parts;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use base64::Engine;
use rand::RngCore;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::models::User;
use crate::state::AppState;

pub const TOKEN_PREFIX: &str = "prexiv_";
const DISPLAY_PREFIX_CHARS: usize = 14;

/// Mint a fresh API token. Plaintext only — caller must hash with
/// `hash_token` before storing in the DB.
pub fn generate_token() -> String {
    let mut bytes = [0u8; 27];
    rand::thread_rng().fill_bytes(&mut bytes);
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    format!("{TOKEN_PREFIX}{b64}")
}

/// SHA-256 hex of the plaintext token.
pub fn hash_token(plain: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(plain.as_bytes());
    hex::encode(hasher.finalize())
}

/// Short non-secret identifier for UI lists and audit logs.
pub fn token_display_prefix(plain: &str) -> String {
    plain.chars().take(DISPLAY_PREFIX_CHARS).collect()
}

/// Detect accidental API-token placement in URLs. Bearer tokens must travel in
/// the Authorization header because URLs are routinely captured in browser
/// history, reverse-proxy logs, Referer headers, and screenshots.
pub fn bearer_token_in_query(parts: &Parts) -> bool {
    parts
        .uri
        .query()
        .map(|q| q.contains(TOKEN_PREFIX))
        .unwrap_or(false)
}

pub enum BearerToken {
    Missing,
    Malformed,
    Present(String),
}

pub fn extract_bearer(parts: &Parts) -> BearerToken {
    let Some(raw) = parts.headers.get(header::AUTHORIZATION) else {
        return BearerToken::Missing;
    };
    let Ok(h) = raw.to_str() else {
        return BearerToken::Malformed;
    };
    let mut it = h.splitn(2, ' ');
    let Some(scheme) = it.next() else {
        return BearerToken::Malformed;
    };
    if !scheme.eq_ignore_ascii_case("bearer") {
        return BearerToken::Malformed;
    }
    let Some(token) = it.next().map(str::trim).filter(|t| !t.is_empty()) else {
        return BearerToken::Malformed;
    };
    BearerToken::Present(token.to_string())
}

/// Look up the user that owns the given plaintext bearer token. Honours
/// `expires_at` (returns None if expired). Touches `last_used_at` on a
/// successful match.
pub async fn find_user_by_bearer(pool: &DbPool, plain: &str) -> Option<User> {
    let h = hash_token(plain);
    let row = sqlx::query_as::<_, (i64, i64, Option<String>)>(crate::db::pg(
        r#"SELECT t.id, t.user_id, t.expires_at
           FROM api_tokens t
           WHERE t.token_hash = ?
           LIMIT 1"#,
    ))
    .bind(&h)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()?;
    let (token_id, user_id, expires_at): (i64, i64, Option<String>) = row;

    // Expiry check against database timestamps.
    if let Some(exp) = expires_at {
        if let Ok(t) = chrono::NaiveDateTime::parse_from_str(&exp, "%Y-%m-%d %H:%M:%S") {
            if t < chrono::Utc::now().naive_utc() {
                return None;
            }
        }
    }

    let mut user = sqlx::query_as::<_, User>(crate::db::pg(
        r#"SELECT id, username, email, display_name, affiliation, bio,
                  karma, is_admin, email_verified, orcid, created_at,
                  email_enc, orcid_verified, institutional_email,
                  orcid_oauth_verified, orcid_oauth_verified_at, orcid_oauth_sub,
                  github_oauth_verified, github_oauth_verified_at, github_id, github_login
           FROM users WHERE id = ?"#,
    ))
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()?;
    user.resolve_email();

    // Only bump last_used_at once we know the token resolved to a real,
    // still-existing user. Avoids touching the row on tokens that point at
    // deleted accounts or on errors further down the request.
    let _ = sqlx::query(crate::db::pg(
        "UPDATE api_tokens SET last_used_at = CURRENT_TIMESTAMP WHERE id = ?",
    ))
    .bind(token_id)
    .execute(pool)
    .await;

    Some(user)
}

/// Required-bearer extractor. Use on agent-only endpoints; returns 401
/// JSON if no valid token is present.
pub struct ApiUser(pub User);

impl<S> FromRequestParts<S> for ApiUser
where
    AppState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = Response;
    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let app: AppState = AppState::from_ref(state);
        let plain = match extract_bearer(parts) {
            BearerToken::Present(t) => t,
            BearerToken::Missing if bearer_token_in_query(parts) => {
                return Err((
                    StatusCode::BAD_REQUEST,
                    Json(json!({
                        "error": "bearer tokens are not accepted in URLs",
                        "hint": "send `Authorization: Bearer prexiv_…`; never put an API token in a query string"
                    })),
                )
                    .into_response());
            }
            BearerToken::Missing | BearerToken::Malformed => {
                return Err((
                    StatusCode::UNAUTHORIZED,
                    Json(json!({
                        "error": "missing or malformed Authorization header",
                        "hint": "send `Authorization: Bearer prexiv_…` — mint a token at /me/tokens"
                    })),
                )
                    .into_response());
            }
        };
        match find_user_by_bearer(&app.pool, &plain).await {
            Some(u) => Ok(ApiUser(u)),
            None => Err((
                StatusCode::UNAUTHORIZED,
                Json(json!({"error": "invalid or expired bearer token"})),
            )
                .into_response()),
        }
    }
}

/// Same as ApiUser but the user must have account-control verification
/// through GitHub OAuth, ORCID OAuth, or email, unless they are an admin.
pub struct ApiVerifiedUser(pub User);

impl<S> FromRequestParts<S> for ApiVerifiedUser
where
    AppState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = Response;
    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let ApiUser(u) = ApiUser::from_request_parts(parts, state).await?;
        if u.is_verified_or_admin() {
            Ok(ApiVerifiedUser(u))
        } else {
            Err((
                StatusCode::FORBIDDEN,
                Json(json!({
                    "error": "account not verified — connect GitHub, connect ORCID, or verify email first"
                })),
            )
                .into_response())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;

    #[test]
    fn generated_tokens_store_only_hash_and_display_prefix() {
        let token = generate_token();
        let hash = hash_token(&token);
        let prefix = token_display_prefix(&token);

        assert!(token.starts_with(TOKEN_PREFIX));
        assert_ne!(hash, token);
        assert_eq!(hash.len(), 64);
        assert!(token.starts_with(&prefix));
        assert!(prefix.len() < token.len());
    }

    #[test]
    fn bearer_extraction_ignores_query_tokens() {
        let (parts, _) = Request::builder()
            .uri("/api/v1/me?access_token=prexiv_leaked")
            .body(Body::empty())
            .unwrap()
            .into_parts();

        assert!(bearer_token_in_query(&parts));
        assert!(matches!(extract_bearer(&parts), BearerToken::Missing));
    }

    #[test]
    fn bearer_extraction_accepts_authorization_header() {
        let (parts, _) = Request::builder()
            .uri("/api/v1/me")
            .header(header::AUTHORIZATION, "Bearer prexiv_abc")
            .body(Body::empty())
            .unwrap()
            .into_parts();

        match extract_bearer(&parts) {
            BearerToken::Present(token) => assert_eq!(token, "prexiv_abc"),
            _ => panic!("expected bearer token"),
        }
    }
}
