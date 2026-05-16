//! /me/email and /confirm-email-change/{token}.
//!
//! Email change is a high-trust operation: an attacker with the session
//! could otherwise change the email to one they control, then trigger a
//! password reset to gain permanent ownership of the account. To raise
//! the bar:
//!
//!   * Current password must be verified (timing-safe).
//!   * Confirmation is mailed to the NEW address, not the old one — the
//!     user must prove they actually control the address they typed.
//!     On confirmation, users.email is replaced AND email_verified is
//!     set to 1 (the click is the verification).
//!   * Only one pending change per user at a time; minting deletes any
//!     prior pending row.
//!   * Email uniqueness is re-checked at consume time, not just at
//!     mint time, so a concurrent registration can't collide silently.

use axum::extract::{Form, Path, State};
use axum::response::{Html, IntoResponse, Redirect, Response};
use maud::html;
use serde::Deserialize;
use tower_sessions::Session;

use crate::auth::{verify_csrf, verify_password_timing_safe, MaybeUser, RequireUser};
use crate::email_change;
use crate::error::AppResult;
use crate::helpers::{build_ctx, clear_session_secret, set_flash, set_session_secret};
use crate::state::AppState;
use crate::templates;
use crate::templates::layout::layout;

// ─── GET /me/email ─────────────────────────────────────────────────────────

pub async fn show(
    State(state): State<AppState>,
    session: Session,
    maybe_user: MaybeUser,
    RequireUser(user): RequireUser,
) -> AppResult<Html<String>> {
    let pending = email_change::pending_for_user(&state.pool, user.id)
        .await
        .ok()
        .flatten();
    let mut ctx = build_ctx(&session, maybe_user, "/me/email").await;
    ctx.no_index = true;
    Ok(Html(
        templates::me_email::render(
            &ctx,
            &user.email,
            pending.as_ref().map(|(e, _)| e.as_str()),
            None,
        )
        .into_string(),
    ))
}

// ─── POST /me/email ────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct ChangeForm {
    pub csrf_token: String,
    pub new_email: String,
    pub current_password: String,
}

pub async fn submit(
    State(state): State<AppState>,
    session: Session,
    maybe_user: MaybeUser,
    RequireUser(user): RequireUser,
    Form(form): Form<ChangeForm>,
) -> AppResult<Response> {
    let render_err = async |msg: &str, maybe_user: MaybeUser| -> Response {
        let pending = email_change::pending_for_user(&state.pool, user.id)
            .await
            .ok()
            .flatten();
        let mut ctx = build_ctx(&session, maybe_user, "/me/email").await;
        ctx.no_index = true;
        Html(
            templates::me_email::render(
                &ctx,
                &user.email,
                pending.as_ref().map(|(e, _)| e.as_str()),
                Some(msg),
            )
            .into_string(),
        )
        .into_response()
    };

    if !verify_csrf(&session, &form.csrf_token).await {
        return Ok(render_err("Form expired — please try again.", maybe_user).await);
    }

    let new_email = form.new_email.trim().to_ascii_lowercase();
    if !new_email.contains('@') || new_email.len() < 5 || new_email.len() > 254 {
        return Ok(render_err("New email looks invalid.", maybe_user).await);
    }
    if new_email == user.email.to_ascii_lowercase() {
        return Ok(render_err("That's already your current email.", maybe_user).await);
    }

    // Verify current password (fetch fresh hash — RequireUser's snapshot
    // omits it). Use the timing-safe wrapper just like /me/password.
    let row: Option<(String,)> = sqlx::query_as(crate::db::pg(
        "SELECT password_hash FROM users WHERE id = ?",
    ))
    .bind(user.id)
    .fetch_optional(&state.pool)
    .await?;
    let current_hash = row.map(|(h,)| h);
    if !verify_password_timing_safe(&form.current_password, current_hash.as_deref()) {
        return Ok(render_err("Current password is incorrect.", maybe_user).await);
    }

    // Reject if the new email already belongs to someone else.
    let new_hash = crate::crypto::email_hash(&new_email).to_vec();
    let taken: Option<(i64,)> = sqlx::query_as(crate::db::pg(
        "SELECT id FROM users WHERE email_hash = ? AND id != ? LIMIT 1",
    ))
    .bind(&new_hash)
    .bind(user.id)
    .fetch_optional(&state.pool)
    .await?;
    if taken.is_some() {
        return Ok(render_err(
            "That email address is already in use on another account.",
            maybe_user,
        )
        .await);
    }

    let token = email_change::mint_and_send(
        &state.pool,
        user.id,
        &new_email,
        &user.username,
        state.app_url.as_deref(),
    )
    .await
    .ok();

    // Production confirms ownership through the new mailbox. The inline
    // token fallback is dev-only unless explicitly enabled.
    if crate::email::inline_token_fallback_enabled() {
        if let Some(t) = &token {
            set_session_secret(&session, "pending_email_change_token", t).await;
        }
    }

    set_flash(
        &session,
        format!(
            "Confirmation link generated for {new_email}. Click the button on the next page to finish the change (or open the email we've queued to that address)."
        ),
    ).await;
    Ok(Redirect::to("/me/edit").into_response())
}

// ─── GET /confirm-email-change/{token} ─────────────────────────────────────

pub async fn confirm(
    State(state): State<AppState>,
    session: Session,
    maybe_user: MaybeUser,
    Path(token): Path<String>,
) -> AppResult<Html<String>> {
    let mut ctx = build_ctx(&session, maybe_user, "/confirm-email-change").await;
    ctx.no_index = true;

    let (ok, headline, message) = match email_change::resolve_token(&state.pool, &token).await {
        Ok(Some((token_id, user_id, new_email))) => {
            match email_change::consume_and_apply(&state.pool, token_id, user_id, &new_email).await {
                Ok(true) => {
                    clear_session_secret(&session, "pending_email_change_token").await;
                    clear_session_secret(&session, "pending_verify_token").await;
                    (
                        true,
                        "Email updated",
                        format!("Your account email is now {new_email}, and it's marked verified. Password-reset mail will be sent here from now on."),
                    )
                }
                Ok(false) => (
                    false,
                    "Address already taken",
                    "The address you confirmed has been registered to another account in the meantime. The change has been cancelled — start over with /me/email if you'd like to pick a different one.".to_string(),
                ),
                Err(e) => {
                    tracing::error!(target: "prexiv::email_change", error = %e, "consume failed");
                    (false, "Something went wrong", "We couldn't finalise the change. Try the link again, or request a new one from /me/email.".to_string())
                }
            }
        }
        Ok(None) => (
            false,
            "Link invalid or expired",
            "This confirmation link doesn't match a pending request, or it's older than 24 hours. Start a fresh request from /me/email.".to_string(),
        ),
        Err(e) => {
            tracing::error!(target: "prexiv::email_change", error = %e, "resolve failed");
            (false, "Something went wrong", "We couldn't look the link up just now. Please try again in a moment.".to_string())
        }
    };

    let body = html! {
        section.page-header {
            h1 { (headline) }
            p.muted { (message) }
        }
        @if ok {
            p { a.btn-primary href="/me/edit" { "Back to your profile →" } }
        } @else {
            p { a.btn-secondary href="/me/email" { "Change email" } " " a.btn-secondary href="/me/edit" { "Back to profile" } }
        }
    };
    Ok(Html(layout(headline, &ctx, body).into_string()))
}

// ─── POST /me/email/cancel ─────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct CancelForm {
    pub csrf_token: String,
}

pub async fn cancel(
    State(state): State<AppState>,
    session: Session,
    RequireUser(user): RequireUser,
    Form(form): Form<CancelForm>,
) -> AppResult<Response> {
    if !verify_csrf(&session, &form.csrf_token).await {
        set_flash(&session, "Form expired — please try again.").await;
        return Ok(Redirect::to("/me/edit").into_response());
    }
    let _ = email_change::invalidate_for_user(&state.pool, user.id).await;
    clear_session_secret(&session, "pending_email_change_token").await;
    set_flash(&session, "Pending email change cancelled.").await;
    Ok(Redirect::to("/me/edit").into_response())
}
