//! GET /verify/{token} — consumes an email-verification token and marks
//! the user verified. Idempotent in the trivial sense: a second visit to
//! the same link returns "invalid or expired" (because we delete the row
//! on first redeem).
//!
//! POST /me/resend-verification — CSRF-protected. Invalidates outstanding
//! tokens for the current user and mints+sends a fresh one.

use axum::extract::{Form, Path, State};
use axum::response::{Html, IntoResponse, Redirect, Response};
use maud::html;
use serde::Deserialize;
use tower_sessions::Session;

use crate::auth::{verify_csrf, MaybeUser, RequireUser};
use crate::error::AppResult;
use crate::helpers::{build_ctx, clear_session_secret, set_flash, set_session_secret};
use crate::state::AppState;
use crate::templates::layout::layout;
use crate::verify;

pub async fn show(
    State(state): State<AppState>,
    session: Session,
    maybe_user: MaybeUser,
    Path(token): Path<String>,
) -> AppResult<Html<String>> {
    let mut ctx = build_ctx(&session, maybe_user, "/verify").await;
    ctx.no_index = true;

    let (ok, headline, message) = match verify::resolve_token(&state.pool, &token).await {
        Ok(Some((token_id, user_id))) => {
            match verify::consume(&state.pool, token_id, user_id).await {
                Ok(()) => {
                    // Clear the session's pending verify token — it now
                    // points at a deleted row, and the verify banner
                    // shouldn't render on subsequent unverified-page
                    // visits (the user is now verified anyway).
                    clear_session_secret(&session, "pending_verify_token").await;
                    (
                        true,
                        "Email verified",
                        "Thanks — your email is verified. You can now submit manuscripts and mint API tokens.".to_string(),
                    )
                }
                Err(e) => {
                    tracing::error!(target: "prexiv::verify", error = %e, "consume failed");
                    (
                        false,
                        "Something went wrong",
                        "We couldn't finalise the verification. Please try the link again, or use Resend verification on the /me/edit page.".to_string(),
                    )
                }
            }
        }
        Ok(None) => (
            false,
            "Link invalid or expired",
            "This verification link doesn't match a pending request, or it expired. Go to /me/edit and use Resend verification to get a fresh link.".to_string(),
        ),
        Err(e) => {
            tracing::error!(target: "prexiv::verify", error = %e, "resolve failed");
            (
                false,
                "Something went wrong",
                "We couldn't look the link up just now. Please try again in a moment.".to_string(),
            )
        }
    };

    let body = html! {
        section.page-header {
            h1 { (headline) }
            p.muted { (message) }
        }
        @if ok {
            p { a.btn-primary href="/submit" { "Submit your first manuscript →" } }
        } @else {
            p { a.btn-secondary href="/me/edit" { "Go to /me/edit" } }
        }
    };
    Ok(Html(layout(headline, &ctx, body).into_string()))
}

#[derive(Deserialize)]
pub struct ResendForm {
    pub csrf_token: String,
}

pub async fn resend(
    State(state): State<AppState>,
    session: Session,
    RequireUser(user): RequireUser,
    Form(form): Form<ResendForm>,
) -> AppResult<Response> {
    if !verify_csrf(&session, &form.csrf_token).await {
        set_flash(&session, "Form expired — please try again.").await;
        return Ok(Redirect::to("/me/edit").into_response());
    }
    if user.is_verified() {
        set_flash(&session, "Your email is already verified.").await;
        return Ok(Redirect::to("/me/edit").into_response());
    }

    // Invalidate prior pending tokens so an old link can't beat the new one.
    if let Err(e) = verify::invalidate_for_user(&state.pool, user.id).await {
        tracing::error!(target: "prexiv::verify", error = %e, user_id = user.id, "invalidate failed");
    }

    // Mint a fresh token and send it. Production keeps the plaintext token
    // out of the browser; the inline fallback is dev-only unless explicitly
    // enabled.
    let pending_token = match verify::mint_and_send(
        &state.pool,
        user.id,
        &user.email,
        &user.username,
        state.app_url.as_deref(),
    )
    .await
    {
        Ok(token) => Some(token),
        Err(e) => {
            tracing::error!(target: "prexiv::verify", error = %e, user_id = user.id, "verification email resend failed");
            set_flash(
                &session,
                "We could not send the verification email. The mail provider rejected the message; please contact the operator or try again after mail settings are fixed.",
            )
            .await;
            return Ok(Redirect::to("/me/edit").into_response());
        }
    };
    if crate::email::inline_token_fallback_enabled() {
        if let Some(t) = pending_token {
            set_session_secret(&session, "pending_verify_token", &t).await;
        }
    }

    set_flash(
        &session,
        format!(
            "Fresh verification link sent to {}. Check that inbox to verify ownership.",
            user.email
        ),
    )
    .await;
    Ok(Redirect::to("/me/edit").into_response())
}
