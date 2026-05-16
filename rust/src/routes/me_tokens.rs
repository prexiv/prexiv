//! /me/tokens — human-facing UI for minting and revoking API tokens.
//! The same tokens authenticate the JSON API at /api/v1/*.

use axum::extract::{Form, Path, State};
use axum::response::{Html, IntoResponse, Redirect, Response};
use serde::Deserialize;
use tower_sessions::Session;

use crate::api_auth::{generate_token, hash_token, token_display_prefix};
use crate::auth::{verify_csrf, MaybeUser, RequireUser};
use crate::error::AppResult;
use crate::helpers::{build_ctx, set_flash, set_session_secret, take_session_secret};
use crate::state::AppState;
use crate::templates;

pub struct TokenRow {
    pub id: i64,
    pub token_prefix: Option<String>,
    pub name: Option<String>,
    pub last_used_at: Option<chrono::NaiveDateTime>,
    pub created_at: Option<chrono::NaiveDateTime>,
    pub expires_at: Option<chrono::NaiveDateTime>,
}

pub async fn show(
    State(state): State<AppState>,
    session: Session,
    maybe_user: MaybeUser,
    RequireUser(user): RequireUser,
) -> AppResult<Html<String>> {
    let rows: Vec<TokenRow> = sqlx::query_as::<_, (i64, Option<String>, Option<String>, Option<chrono::NaiveDateTime>, Option<chrono::NaiveDateTime>, Option<chrono::NaiveDateTime>)>(
        crate::db::pg("SELECT id, token_prefix, name, last_used_at, created_at, expires_at FROM api_tokens WHERE user_id = ? ORDER BY created_at DESC"),
    )
    .bind(user.id)
    .fetch_all(&state.pool)
    .await?
    .into_iter()
    .map(|(id, token_prefix, name, last_used_at, created_at, expires_at)| TokenRow {
        id, token_prefix, name, last_used_at, created_at, expires_at,
    })
    .collect();

    let mut ctx = build_ctx(&session, maybe_user, "/me/tokens").await;
    ctx.no_index = true;
    // Pull a one-shot token from the session (set by create()). The token
    // itself is encrypted in the PostgreSQL-backed session store.
    let token = take_session_secret(&session, "just_minted_token").await;
    let token_name = session
        .remove::<String>("just_minted_token_name")
        .await
        .ok()
        .flatten()
        .filter(|s| !s.is_empty());
    let _ = session
        .remove::<(String, Option<String>)>("just_minted_token")
        .await;
    let just_minted = token.map(|t| (t, token_name));
    let base = state.app_url.as_deref().unwrap_or("http://localhost:3001");
    Ok(Html(
        templates::me_tokens::render(
            &ctx,
            &rows,
            just_minted.as_ref(),
            base,
            user.is_verified_or_admin(),
        )
        .into_string(),
    ))
}

#[derive(Deserialize)]
pub struct CreateForm {
    pub csrf_token: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub expires_in_days: String,
}

pub async fn create(
    State(state): State<AppState>,
    session: Session,
    RequireUser(user): RequireUser,
    Form(form): Form<CreateForm>,
) -> AppResult<Response> {
    if !verify_csrf(&session, &form.csrf_token).await {
        set_flash(&session, "Form expired — please try again.").await;
        return Ok(Redirect::to("/me/tokens").into_response());
    }
    if !user.is_verified_or_admin() {
        set_flash(
            &session,
            "Connect GitHub, connect ORCID, or verify email before minting API tokens.",
        )
        .await;
        return Ok(Redirect::to("/me/tokens").into_response());
    }
    let name = form.name.trim();
    let name = if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    };
    let days = form
        .expires_in_days
        .trim()
        .parse::<i64>()
        .ok()
        .filter(|d| *d > 0);
    let expires_at = days.map(|d| (chrono::Utc::now() + chrono::Duration::days(d)).naive_utc());

    let plain = generate_token();
    let hash = hash_token(&plain);
    let prefix = token_display_prefix(&plain);
    sqlx::query(crate::db::pg(
        "INSERT INTO api_tokens (user_id, token_hash, token_prefix, name, expires_at) VALUES (?, ?, ?, ?, ?)",
    ))
    .bind(user.id)
    .bind(&hash)
    .bind(&prefix)
    .bind(name.as_deref())
    .bind(expires_at)
    .execute(&state.pool)
    .await?;
    let _ = sqlx::query(crate::db::pg(
        "INSERT INTO audit_log (actor_user_id, action, target_type, detail) VALUES (?, 'api_token_mint', 'api_token', ?)",
    ))
    .bind(user.id)
    .bind(serde_json::json!({
        "token_prefix": prefix,
        "name": name.as_deref(),
        "expires_at": expires_at,
        "surface": "web"
    }).to_string())
    .execute(&state.pool)
    .await;

    set_session_secret(&session, "just_minted_token", &plain).await;
    let _ = session
        .insert("just_minted_token_name", name.clone().unwrap_or_default())
        .await;
    Ok(Redirect::to("/me/tokens").into_response())
}

#[derive(Deserialize)]
pub struct RevokeForm {
    pub csrf_token: String,
}

pub async fn revoke(
    State(state): State<AppState>,
    session: Session,
    RequireUser(user): RequireUser,
    Path(id): Path<i64>,
    Form(form): Form<RevokeForm>,
) -> AppResult<Response> {
    if !verify_csrf(&session, &form.csrf_token).await {
        set_flash(&session, "Form expired — please try again.").await;
        return Ok(Redirect::to("/me/tokens").into_response());
    }
    let deleted: Option<(Option<String>, Option<String>)> = sqlx::query_as(crate::db::pg(
        "DELETE FROM api_tokens WHERE id = ? AND user_id = ? RETURNING token_prefix, name",
    ))
    .bind(id)
    .bind(user.id)
    .fetch_optional(&state.pool)
    .await?;
    if let Some((token_prefix, name)) = deleted {
        let _ = sqlx::query(crate::db::pg(
            "INSERT INTO audit_log (actor_user_id, action, target_type, target_id, detail) VALUES (?, 'api_token_revoke', 'api_token', ?, ?)",
        ))
        .bind(user.id)
        .bind(id)
        .bind(serde_json::json!({
            "token_prefix": token_prefix,
            "name": name,
            "surface": "web"
        }).to_string())
        .execute(&state.pool)
        .await;
    }
    set_flash(&session, "Token revoked.").await;
    Ok(Redirect::to("/me/tokens").into_response())
}
