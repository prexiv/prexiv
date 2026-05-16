//! /me/edit — real profile editor. Replaces the previous stub.

use axum::extract::{Form, Query, State};
use axum::response::{Html, IntoResponse, Redirect, Response};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use rand::rngs::OsRng;
use rand::RngCore;
use serde::Deserialize;
use tower_sessions::Session;

use crate::auth::{verify_csrf, MaybeUser, RequireUser};
use crate::email_change;
use crate::error::AppResult;
use crate::helpers::{
    build_ctx, set_flash, set_github_flash, set_orcid_flash, take_github_flash, take_orcid_flash,
};
use crate::state::AppState;
use crate::templates;

const ORCID_OAUTH_STATE_KEY: &str = "orcid_oauth_state";
const ORCID_OAUTH_NONCE_KEY: &str = "orcid_oauth_nonce";
const GITHUB_OAUTH_STATE_KEY: &str = "github_oauth_state";

pub struct EditValues {
    pub display_name: String,
    pub affiliation: String,
    pub bio: String,
}

pub async fn show(
    State(state): State<AppState>,
    session: Session,
    maybe_user: MaybeUser,
    RequireUser(u): RequireUser,
) -> AppResult<Html<String>> {
    let values = EditValues {
        display_name: u.display_name.clone().unwrap_or_default(),
        affiliation: u.affiliation.clone().unwrap_or_default(),
        bio: u.bio.clone().unwrap_or_default(),
    };
    let pending_email = email_change::pending_for_user(&state.pool, u.id)
        .await
        .ok()
        .flatten()
        .map(|(addr, _)| addr);
    let orcid_flash = take_orcid_flash(&session).await;
    let github_flash = take_github_flash(&session).await;
    let github_oauth_unavailable = github_oauth_unavailable_message(&state);
    let orcid_oauth_unavailable = orcid_oauth_unavailable_message(&state);
    let mut ctx = build_ctx(&session, maybe_user, "/me/edit").await;
    ctx.no_index = true;
    Ok(Html(
        templates::me_edit::render(
            &ctx,
            &values,
            &[],
            pending_email.as_deref(),
            templates::me_edit::VerificationUi {
                github_flash: github_flash.as_ref().map(|(m, e)| (m.as_str(), *e)),
                github_oauth_unavailable: github_oauth_unavailable.as_deref(),
                orcid_flash: orcid_flash.as_ref().map(|(m, e)| (m.as_str(), *e)),
                orcid_oauth_unavailable: orcid_oauth_unavailable.as_deref(),
            },
        )
        .into_string(),
    ))
}

#[derive(Deserialize)]
pub struct EditForm {
    pub csrf_token: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub affiliation: String,
    #[serde(default)]
    pub bio: String,
}

pub async fn submit(
    State(state): State<AppState>,
    session: Session,
    maybe_user: MaybeUser,
    RequireUser(u): RequireUser,
    Form(form): Form<EditForm>,
) -> AppResult<Response> {
    if !verify_csrf(&session, &form.csrf_token).await {
        set_flash(&session, "Form expired — please try again.").await;
        return Ok(Redirect::to("/me/edit").into_response());
    }

    let display_name = form.display_name.trim();
    let affiliation = form.affiliation.trim();
    let bio = form.bio.trim();

    let mut errors: Vec<String> = vec![];
    if display_name.len() > 200 {
        errors.push("Display name must be ≤200 chars".into());
    }
    if affiliation.len() > 200 {
        errors.push("Affiliation must be ≤200 chars".into());
    }
    if bio.len() > 2000 {
        errors.push("Bio must be ≤2000 chars".into());
    }
    if !errors.is_empty() {
        let values = EditValues {
            display_name: form.display_name.clone(),
            affiliation: form.affiliation.clone(),
            bio: form.bio.clone(),
        };
        let pending_email = email_change::pending_for_user(&state.pool, u.id)
            .await
            .ok()
            .flatten()
            .map(|(addr, _)| addr);
        let mut ctx = build_ctx(&session, maybe_user, "/me/edit").await;
        ctx.no_index = true;
        let github_oauth_unavailable = github_oauth_unavailable_message(&state);
        let orcid_oauth_unavailable = orcid_oauth_unavailable_message(&state);
        return Ok(Html(
            templates::me_edit::render(
                &ctx,
                &values,
                &errors,
                pending_email.as_deref(),
                templates::me_edit::VerificationUi {
                    github_flash: None,
                    github_oauth_unavailable: github_oauth_unavailable.as_deref(),
                    orcid_flash: None,
                    orcid_oauth_unavailable: orcid_oauth_unavailable.as_deref(),
                },
            )
            .into_string(),
        )
        .into_response());
    }

    sqlx::query(crate::db::pg(
        "UPDATE users SET display_name = ?, affiliation = ?, bio = ?
          WHERE id = ?",
    ))
    .bind(opt(display_name))
    .bind(opt(affiliation))
    .bind(opt(bio))
    .bind(u.id)
    .execute(&state.pool)
    .await?;
    set_flash(&session, "Profile updated.").await;
    Ok(Redirect::to(&format!("/u/{}", u.username)).into_response())
}

pub async fn connect_github(
    State(state): State<AppState>,
    session: Session,
    RequireUser(_u): RequireUser,
) -> AppResult<Response> {
    let cfg = match crate::github::oauth_config(state.app_url.as_deref()) {
        Ok(Some(cfg)) => cfg,
        Ok(None) => {
            set_github_flash(
                &session,
                "GitHub sign-in is not configured yet. Set GITHUB_CLIENT_ID and GITHUB_CLIENT_SECRET on the server.",
                true,
            )
            .await;
            return Ok(Redirect::to("/me/edit").into_response());
        }
        Err(e) => {
            set_github_flash(
                &session,
                format!("GitHub sign-in configuration error: {e}"),
                true,
            )
            .await;
            return Ok(Redirect::to("/me/edit").into_response());
        }
    };
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    let oauth_state = URL_SAFE_NO_PAD.encode(bytes);
    session
        .insert(GITHUB_OAUTH_STATE_KEY, oauth_state.clone())
        .await
        .map_err(|e| crate::error::AppError::Other(anyhow::anyhow!(e)))?;
    Ok(Redirect::to(&cfg.authorize_url(&oauth_state)).into_response())
}

#[derive(Deserialize)]
pub struct GithubCallbackQuery {
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub error_description: Option<String>,
}

pub async fn github_callback(
    State(state): State<AppState>,
    session: Session,
    RequireUser(u): RequireUser,
    Query(q): Query<GithubCallbackQuery>,
) -> AppResult<Response> {
    let expected_state: Option<String> = session
        .get(GITHUB_OAUTH_STATE_KEY)
        .await
        .map_err(|e| crate::error::AppError::Other(anyhow::anyhow!(e)))?;
    let _ = session.remove::<String>(GITHUB_OAUTH_STATE_KEY).await;

    if let Some(err) = q.error.as_deref() {
        let msg = q
            .error_description
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(err);
        set_github_flash(
            &session,
            format!("GitHub sign-in was not completed: {msg}"),
            true,
        )
        .await;
        return Ok(Redirect::to("/me/edit").into_response());
    }

    let Some(expected_state) = expected_state else {
        set_github_flash(
            &session,
            "GitHub sign-in state was missing. Start again from the Connect with GitHub button.",
            true,
        )
        .await;
        return Ok(Redirect::to("/me/edit").into_response());
    };
    if q.state.as_deref() != Some(expected_state.as_str()) {
        set_github_flash(
            &session,
            "GitHub sign-in state did not match. Start again from the Connect with GitHub button.",
            true,
        )
        .await;
        return Ok(Redirect::to("/me/edit").into_response());
    }
    let Some(code) = q.code.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
        set_github_flash(
            &session,
            "GitHub did not return an authorization code.",
            true,
        )
        .await;
        return Ok(Redirect::to("/me/edit").into_response());
    };
    let cfg = match crate::github::oauth_config(state.app_url.as_deref()) {
        Ok(Some(cfg)) => cfg,
        Ok(None) => {
            set_github_flash(
                &session,
                "GitHub sign-in is not configured on the server anymore. Try again later.",
                true,
            )
            .await;
            return Ok(Redirect::to("/me/edit").into_response());
        }
        Err(e) => {
            set_github_flash(
                &session,
                format!("GitHub sign-in configuration error: {e}"),
                true,
            )
            .await;
            return Ok(Redirect::to("/me/edit").into_response());
        }
    };
    let authenticated = match crate::github::exchange_authorization_code(&cfg, code).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(user_id = u.id, error = %e, "GitHub OAuth exchange failed");
            set_github_flash(
                &session,
                "GitHub sign-in failed while exchanging the authorization code. Try again in a minute.",
                true,
            )
            .await;
            return Ok(Redirect::to("/me/edit").into_response());
        }
    };
    let existing: Option<(i64, String)> = sqlx::query_as(crate::db::pg(
        "SELECT id, username FROM users
          WHERE github_id = ? AND id != ?
          LIMIT 1",
    ))
    .bind(&authenticated.id)
    .bind(u.id)
    .fetch_optional(&state.pool)
    .await?;
    if let Some((_id, username)) = existing {
        set_github_flash(
            &session,
            format!(
                "GitHub account @{} is already connected to account @{username}. Disconnect it there or contact an admin.",
                authenticated.login
            ),
            true,
        )
        .await;
        return Ok(Redirect::to("/me/edit").into_response());
    }

    sqlx::query(crate::db::pg(
        "UPDATE users
            SET github_id = ?,
                github_login = ?,
                github_oauth_verified = 1,
                github_oauth_verified_at = CURRENT_TIMESTAMP
          WHERE id = ?",
    ))
    .bind(&authenticated.id)
    .bind(&authenticated.login)
    .bind(u.id)
    .execute(&state.pool)
    .await?;

    set_github_flash(
        &session,
        format!(
            "GitHub account @{} connected. Your account can now submit, comment, vote, and mint API tokens.",
            authenticated.login
        ),
        false,
    )
    .await;
    Ok(Redirect::to("/me/edit").into_response())
}

pub async fn connect_orcid(
    State(state): State<AppState>,
    session: Session,
    RequireUser(_u): RequireUser,
) -> AppResult<Response> {
    let cfg = match crate::orcid::oauth_config(state.app_url.as_deref()) {
        Ok(Some(cfg)) => cfg,
        Ok(None) => {
            set_orcid_flash(
                &session,
                "ORCID OAuth is not configured yet. Set ORCID_CLIENT_ID, ORCID_CLIENT_SECRET, and ORCID_REDIRECT_URI on the server.",
                true,
            )
            .await;
            return Ok(Redirect::to("/me/edit").into_response());
        }
        Err(e) => {
            set_orcid_flash(
                &session,
                format!("ORCID OAuth configuration error: {e}"),
                true,
            )
            .await;
            return Ok(Redirect::to("/me/edit").into_response());
        }
    };
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    let oauth_state = URL_SAFE_NO_PAD.encode(bytes);
    let mut nonce_bytes = [0u8; 32];
    OsRng.fill_bytes(&mut nonce_bytes);
    let oauth_nonce = URL_SAFE_NO_PAD.encode(nonce_bytes);
    session
        .insert(ORCID_OAUTH_STATE_KEY, oauth_state.clone())
        .await
        .map_err(|e| crate::error::AppError::Other(anyhow::anyhow!(e)))?;
    session
        .insert(ORCID_OAUTH_NONCE_KEY, oauth_nonce.clone())
        .await
        .map_err(|e| crate::error::AppError::Other(anyhow::anyhow!(e)))?;
    Ok(Redirect::to(&cfg.authorize_url(&oauth_state, &oauth_nonce)).into_response())
}

#[derive(Deserialize)]
pub struct OrcidCallbackQuery {
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub error_description: Option<String>,
}

pub async fn orcid_callback(
    State(state): State<AppState>,
    session: Session,
    RequireUser(u): RequireUser,
    Query(q): Query<OrcidCallbackQuery>,
) -> AppResult<Response> {
    let expected_state: Option<String> = session
        .get(ORCID_OAUTH_STATE_KEY)
        .await
        .map_err(|e| crate::error::AppError::Other(anyhow::anyhow!(e)))?;
    let expected_nonce: Option<String> = session
        .get(ORCID_OAUTH_NONCE_KEY)
        .await
        .map_err(|e| crate::error::AppError::Other(anyhow::anyhow!(e)))?;
    let _ = session.remove::<String>(ORCID_OAUTH_STATE_KEY).await;
    let _ = session.remove::<String>(ORCID_OAUTH_NONCE_KEY).await;

    if let Some(err) = q.error.as_deref() {
        let msg = q
            .error_description
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(err);
        set_orcid_flash(
            &session,
            format!("ORCID sign-in was not completed: {msg}"),
            true,
        )
        .await;
        return Ok(Redirect::to("/me/edit").into_response());
    }

    let Some(expected_state) = expected_state else {
        set_orcid_flash(
            &session,
            "ORCID sign-in state was missing. Start again from the Connect with ORCID button.",
            true,
        )
        .await;
        return Ok(Redirect::to("/me/edit").into_response());
    };
    let Some(expected_nonce) = expected_nonce else {
        set_orcid_flash(
            &session,
            "ORCID sign-in nonce was missing. Start again from the Connect with ORCID button.",
            true,
        )
        .await;
        return Ok(Redirect::to("/me/edit").into_response());
    };
    if q.state.as_deref() != Some(expected_state.as_str()) {
        set_orcid_flash(
            &session,
            "ORCID sign-in state did not match. Start again from the Connect with ORCID button.",
            true,
        )
        .await;
        return Ok(Redirect::to("/me/edit").into_response());
    }
    let Some(code) = q.code.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
        set_orcid_flash(
            &session,
            "ORCID did not return an authorization code.",
            true,
        )
        .await;
        return Ok(Redirect::to("/me/edit").into_response());
    };
    let cfg = match crate::orcid::oauth_config(state.app_url.as_deref()) {
        Ok(Some(cfg)) => cfg,
        Ok(None) => {
            set_orcid_flash(
                &session,
                "ORCID OAuth is not configured on the server anymore. Try again later.",
                true,
            )
            .await;
            return Ok(Redirect::to("/me/edit").into_response());
        }
        Err(e) => {
            set_orcid_flash(
                &session,
                format!("ORCID OAuth configuration error: {e}"),
                true,
            )
            .await;
            return Ok(Redirect::to("/me/edit").into_response());
        }
    };
    let authenticated = match crate::orcid::exchange_authorization_code(&cfg, code, &expected_nonce)
        .await
    {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(user_id = u.id, error = %e, "ORCID OAuth exchange failed");
            set_orcid_flash(
                &session,
                "ORCID sign-in failed while exchanging the authorization code. Try again in a minute.",
                true,
            )
            .await;
            return Ok(Redirect::to("/me/edit").into_response());
        }
    };
    let existing: Option<(i64, String)> = sqlx::query_as(crate::db::pg(
        "SELECT id, username FROM users
          WHERE orcid_oauth_sub = ? AND id != ?
          LIMIT 1",
    ))
    .bind(&authenticated.orcid)
    .bind(u.id)
    .fetch_optional(&state.pool)
    .await?;
    if let Some((_id, username)) = existing {
        set_orcid_flash(
            &session,
            format!(
                "ORCID iD {} is already connected to account @{username}. Disconnect it there or contact an admin.",
                authenticated.orcid
            ),
            true,
        )
        .await;
        return Ok(Redirect::to("/me/edit").into_response());
    }

    sqlx::query(crate::db::pg(
        "UPDATE users
            SET orcid = ?,
                orcid_oauth_sub = ?,
                orcid_oauth_verified = 1,
                orcid_oauth_verified_at = CURRENT_TIMESTAMP,
                orcid_verified = 0
          WHERE id = ?",
    ))
    .bind(&authenticated.orcid)
    .bind(&authenticated.orcid)
    .bind(u.id)
    .execute(&state.pool)
    .await?;

    let who = authenticated
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|name| format!(" as {name}"))
        .unwrap_or_default();
    set_orcid_flash(
        &session,
        format!(
            "ORCID iD {} authenticated{who}. Your account can now submit, appear in default listings, comment, vote, and mint API tokens.",
            authenticated.orcid
        ),
        false,
    )
    .await;
    Ok(Redirect::to("/me/edit").into_response())
}

fn opt(s: &str) -> Option<&str> {
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn orcid_oauth_unavailable_message(state: &AppState) -> Option<String> {
    match crate::orcid::oauth_config(state.app_url.as_deref()) {
        Ok(Some(_)) => None,
        Ok(None) => Some(
            "ORCID sign-in is not configured on this server yet. Add ORCID_CLIENT_ID and ORCID_CLIENT_SECRET to enable ownership-grade ORCID binding."
                .to_string(),
        ),
        Err(_) => Some(
            "ORCID sign-in is configured incorrectly on this server. Check ORCID_CLIENT_ID, ORCID_CLIENT_SECRET, and ORCID_REDIRECT_URI."
                .to_string(),
        ),
    }
}

fn github_oauth_unavailable_message(state: &AppState) -> Option<String> {
    match crate::github::oauth_config(state.app_url.as_deref()) {
        Ok(Some(_)) => None,
        Ok(None) => Some(
            "GitHub sign-in is not configured on this server yet. Add GITHUB_CLIENT_ID and GITHUB_CLIENT_SECRET to enable account verification."
                .to_string(),
        ),
        Err(_) => Some(
            "GitHub sign-in is configured incorrectly on this server. Check GITHUB_CLIENT_ID, GITHUB_CLIENT_SECRET, and GITHUB_REDIRECT_URI."
                .to_string(),
        ),
    }
}
