//! Shared helpers for handlers: building `PageCtx`, flash messages.

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use tower_sessions::Session;

use crate::auth::{csrf_token, MaybeUser};
use crate::state::AppState;
use crate::templates::PageCtx;

const SESSION_FLASH_KEY: &str = "flash";

fn encrypted_session_key(key: &str) -> String {
    format!("{key}_enc")
}

fn decrypt_session_secret(encoded: &str) -> Option<String> {
    BASE64_STANDARD
        .decode(encoded)
        .ok()
        .and_then(|blob| crate::crypto::decrypt_blob(&blob).ok())
        .and_then(|plain| String::from_utf8(plain).ok())
}

pub async fn set_session_secret(session: &Session, key: &str, plaintext: &str) {
    let enc_key = encrypted_session_key(key);
    if let Ok(blob) = crate::crypto::encrypt_blob(plaintext.as_bytes()) {
        let _ = session.insert(&enc_key, BASE64_STANDARD.encode(blob)).await;
    }
    let _ = session.remove::<String>(key).await;
}

pub async fn get_session_secret(session: &Session, key: &str) -> Option<String> {
    let enc_key = encrypted_session_key(key);
    if let Ok(Some(encoded)) = session.get::<String>(&enc_key).await {
        return decrypt_session_secret(&encoded);
    }
    let legacy: Option<String> = session.get::<String>(key).await.ok().flatten();
    if let Some(plaintext) = legacy.as_deref() {
        set_session_secret(session, key, plaintext).await;
    }
    legacy
}

pub async fn take_session_secret(session: &Session, key: &str) -> Option<String> {
    let value = get_session_secret(session, key).await;
    clear_session_secret(session, key).await;
    value
}

pub async fn clear_session_secret(session: &Session, key: &str) {
    let enc_key = encrypted_session_key(key);
    let _ = session.remove::<String>(&enc_key).await;
    let _ = session.remove::<String>(key).await;
}

/// Read a one-shot flash message from the session — also clears it.
pub async fn take_flash(session: &Session) -> Option<String> {
    let msg: Option<String> = session.get(SESSION_FLASH_KEY).await.ok().flatten();
    if msg.is_some() {
        let _ = session.remove::<String>(SESSION_FLASH_KEY).await;
    }
    msg
}

pub async fn set_flash(session: &Session, msg: impl Into<String>) {
    let _ = session.insert(SESSION_FLASH_KEY, msg.into()).await;
}

// ─── ORCID OAuth inline feedback ──────────────────────────────────────
//
// The ORCID OAuth callbacks always redirect back to /me/edit. The
// global flash above renders at the top of the page where it's easy
// to miss when the user is focused on the ORCID section near the
// middle of the form. We stash ORCID feedback under a separate session
// key and render it inline inside the verification status panel.
//
// The value is prefixed `ok:` or `err:` so the renderer can paint the
// success / error states distinctly without a second session lookup.

const SESSION_ORCID_FLASH_KEY: &str = "orcid_verify_flash";

pub async fn take_orcid_flash(session: &Session) -> Option<(String, bool)> {
    let raw: Option<String> = session.get(SESSION_ORCID_FLASH_KEY).await.ok().flatten();
    if raw.is_some() {
        let _ = session.remove::<String>(SESSION_ORCID_FLASH_KEY).await;
    }
    raw.map(|s| {
        if let Some(rest) = s.strip_prefix("ok:") {
            (rest.to_string(), false)
        } else if let Some(rest) = s.strip_prefix("err:") {
            (rest.to_string(), true)
        } else {
            (s, true)
        }
    })
}

pub async fn set_orcid_flash(session: &Session, msg: impl Into<String>, is_error: bool) {
    let prefix = if is_error { "err:" } else { "ok:" };
    let _ = session
        .insert(SESSION_ORCID_FLASH_KEY, format!("{prefix}{}", msg.into()))
        .await;
}

// ─── GitHub OAuth inline feedback ─────────────────────────────────────

const SESSION_GITHUB_FLASH_KEY: &str = "github_verify_flash";

pub async fn take_github_flash(session: &Session) -> Option<(String, bool)> {
    let raw: Option<String> = session.get(SESSION_GITHUB_FLASH_KEY).await.ok().flatten();
    if raw.is_some() {
        let _ = session.remove::<String>(SESSION_GITHUB_FLASH_KEY).await;
    }
    raw.map(|s| {
        if let Some(rest) = s.strip_prefix("ok:") {
            (rest.to_string(), false)
        } else if let Some(rest) = s.strip_prefix("err:") {
            (rest.to_string(), true)
        } else {
            (s, true)
        }
    })
}

pub async fn set_github_flash(session: &Session, msg: impl Into<String>, is_error: bool) {
    let prefix = if is_error { "err:" } else { "ok:" };
    let _ = session
        .insert(SESSION_GITHUB_FLASH_KEY, format!("{prefix}{}", msg.into()))
        .await;
}

pub async fn build_ctx(
    session: &Session,
    MaybeUser(user): MaybeUser,
    current_path: impl Into<String>,
) -> PageCtx {
    let csrf_token = csrf_token(session).await;
    let flash = take_flash(session).await;
    // Persistent across requests until the user verifies. We *don't*
    // remove it here (unlike `flash`) — we want the inline link to
    // remain available if the user reloads or navigates between
    // unverified pages. Cleared by the /verify/{token} handler on
    // successful redeem.
    let pending_verify_token = get_session_secret(session, "pending_verify_token").await;
    let pending_email_change_token =
        get_session_secret(session, "pending_email_change_token").await;
    PageCtx {
        user,
        csrf_token,
        no_index: false,
        flash,
        current_path: current_path.into(),
        pending_verify_token,
        pending_email_change_token,
        unread_notifications: 0,
        og: None,
        jsonld: None,
        canonical_url: None,
    }
}

/// Variant of build_ctx that also fetches the unread-notification count
/// for the logged-in user, so the topbar bell badge is populated. Use
/// from routes that pass through state and want the bell visible.
pub async fn build_ctx_with_state(
    state: &AppState,
    session: &Session,
    maybe_user: MaybeUser,
    current_path: impl Into<String>,
) -> PageCtx {
    let user_id = maybe_user.0.as_ref().map(|u| u.id);
    let mut ctx = build_ctx(session, maybe_user, current_path).await;
    if let Some(uid) = user_id {
        ctx.unread_notifications = crate::notifications::unread_count(&state.pool, uid)
            .await
            .unwrap_or(0);
    }
    ctx
}
