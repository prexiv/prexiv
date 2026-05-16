#![allow(clippy::type_complexity)]
//! /me/delete-account + /me/export — GDPR-style account-management.
//!
//! Delete: hard-deletes the user row + anything that cascades (sessions,
//! tokens, follows, votes, notifications, password-reset / email-verify
//! tokens, user_totp). Manuscripts and comments are anonymized — their
//! foreign keys are repointed at a placeholder `[deleted]` user so the
//! research content stays citable but no longer links to the person.
//!
//! Export: returns a single JSON blob with the user's profile, every
//! manuscript they submitted (full body), every comment they wrote,
//! their vote history, and the people they follow / who follow them.
//! Served with Content-Disposition: attachment so the browser saves.

use axum::extract::{Form, State};
use axum::http::header;
use axum::response::{Html, IntoResponse, Redirect, Response};
use serde::Deserialize;
use serde_json::json;
use tower_sessions::Session;

use crate::auth::{
    logout_session, verify_csrf, verify_password_timing_safe, MaybeUser, RequireUser,
};
use crate::error::AppResult;
use crate::helpers::{build_ctx, set_flash};
use crate::state::AppState;
use crate::templates;

// ── /me/delete-account GET ──────────────────────────────────────────

pub async fn show_delete(
    session: Session,
    maybe_user: MaybeUser,
    RequireUser(_user): RequireUser,
) -> AppResult<Html<String>> {
    let mut ctx = build_ctx(&session, maybe_user, "/me/delete-account").await;
    ctx.no_index = true;
    Ok(Html(
        templates::me_account::render_delete(&ctx, None).into_string(),
    ))
}

#[derive(Deserialize)]
pub struct DeleteForm {
    pub csrf_token: String,
    pub current_password: String,
    pub confirm_username: String,
}

pub async fn submit_delete(
    State(state): State<AppState>,
    session: Session,
    maybe_user: MaybeUser,
    RequireUser(user): RequireUser,
    Form(form): Form<DeleteForm>,
) -> AppResult<Response> {
    let render_err = async |msg: &str, maybe_user: MaybeUser| -> Response {
        let mut ctx = build_ctx(&session, maybe_user, "/me/delete-account").await;
        ctx.no_index = true;
        Html(templates::me_account::render_delete(&ctx, Some(msg)).into_string()).into_response()
    };

    if !verify_csrf(&session, &form.csrf_token).await {
        return Ok(render_err("Form expired — please try again.", maybe_user).await);
    }
    if form.confirm_username.trim() != user.username {
        return Ok(render_err(
            "Confirmation didn't match — you have to type your username exactly to proceed.",
            maybe_user,
        )
        .await);
    }
    let hash: Option<(String,)> = sqlx::query_as(crate::db::pg(
        "SELECT password_hash FROM users WHERE id = ?",
    ))
    .bind(user.id)
    .fetch_optional(&state.pool)
    .await?;
    if !verify_password_timing_safe(&form.current_password, hash.as_ref().map(|(h,)| h.as_str())) {
        return Ok(render_err("Current password is incorrect.", maybe_user).await);
    }

    // Anonymize then delete. We do everything in one transaction so a
    // failure rolls back cleanly.
    let mut tx = state.pool.begin().await?;
    let (placeholder_id,): (i64,) = sqlx::query_as(crate::db::pg(
        r#"INSERT INTO users (username, email, password_hash, display_name, email_verified)
           VALUES ('[deleted]', '', '!', '[deleted]', 0)
           ON CONFLICT(username) DO UPDATE SET email = excluded.email
           RETURNING id"#,
    ))
    .fetch_one(&mut *tx)
    .await?;

    sqlx::query(crate::db::pg(
        "UPDATE manuscripts SET submitter_id = ? WHERE submitter_id = ?",
    ))
    .bind(placeholder_id)
    .bind(user.id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(crate::db::pg(
        "UPDATE comments SET author_id = ? WHERE author_id = ?",
    ))
    .bind(placeholder_id)
    .bind(user.id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(crate::db::pg(
        "UPDATE audit_log SET actor_user_id = NULL WHERE actor_user_id = ?",
    ))
    .bind(user.id)
    .execute(&mut *tx)
    .await?;
    // Everything else (tokens, follows, votes, notifications, etc.)
    // cascades via FKs in the migrations.
    sqlx::query(crate::db::pg("DELETE FROM users WHERE id = ?"))
        .bind(user.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    let _ = logout_session(&session).await;
    set_flash(&session, "Account deleted. Your manuscripts and comments remain on the site but are no longer attributed to you.").await;
    Ok(Redirect::to("/").into_response())
}

// ── /me/export ──────────────────────────────────────────────────────

pub async fn export(
    State(state): State<AppState>,
    _session: Session,
    _maybe_user: MaybeUser,
    RequireUser(user): RequireUser,
) -> AppResult<Response> {
    // Profile.
    let profile = json!({
        "id":            user.id,
        "username":      user.username,
        "email":         user.email,
        "display_name":  user.display_name,
        "affiliation":   user.affiliation,
        "bio":           user.bio,
        "orcid":         user.orcid,
        "orcid_oauth_verified": user.is_orcid_oauth_verified(),
        "orcid_oauth_verified_at": user.orcid_oauth_verified_at,
        "github_oauth_verified": user.is_github_oauth_verified(),
        "github_oauth_verified_at": user.github_oauth_verified_at,
        "github_id":      user.github_id,
        "github_login":   user.github_login,
        "karma":         user.karma.unwrap_or(0),
        "account_verified": user.is_account_verified(),
        "email_verified": user.is_verified(),
        "is_admin":       user.is_admin(),
        "created_at":     user.created_at,
    });

    // Manuscripts you submitted.
    let manuscripts: Vec<(
        i64,
        Option<String>,
        Option<String>,
        String,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        Option<chrono::NaiveDateTime>,
        Option<chrono::NaiveDateTime>,
        i64,
        Option<chrono::NaiveDateTime>,
        Option<String>,
        Option<String>,
    )> = sqlx::query_as(crate::db::pg(
        "SELECT id, arxiv_like_id, doi, title, abstract, authors, category,
                    pdf_path, external_url, created_at, updated_at,
                    withdrawn, withdrawn_at, withdrawn_reason, conductor_notes
             FROM manuscripts WHERE submitter_id = ? ORDER BY id",
    ))
    .bind(user.id)
    .fetch_all(&state.pool)
    .await?;
    let ms_json: Vec<serde_json::Value> = manuscripts
        .into_iter()
        .map(|m| {
            json!({
                "id": m.0, "arxiv_like_id": m.1, "doi": m.2, "title": m.3,
                "abstract": m.4, "authors": m.5, "category": m.6,
                "pdf_path": m.7, "external_url": m.8,
                "created_at": m.9, "updated_at": m.10,
                "withdrawn": m.11 != 0, "withdrawn_at": m.12, "withdrawn_reason": m.13,
                "conductor_notes": m.14,
            })
        })
        .collect();

    // Comments you wrote.
    let comments: Vec<(i64, i64, Option<i64>, String, Option<chrono::NaiveDateTime>)> =
        sqlx::query_as(crate::db::pg(
            "SELECT id, manuscript_id, parent_id, content, created_at
             FROM comments WHERE author_id = ? ORDER BY id",
        ))
        .bind(user.id)
        .fetch_all(&state.pool)
        .await?;
    let cm_json: Vec<serde_json::Value> = comments.into_iter().map(|c| json!({
        "id": c.0, "manuscript_id": c.1, "parent_id": c.2, "content": c.3, "created_at": c.4,
    })).collect();

    // Votes.
    let votes: Vec<(String, i64, i64, Option<chrono::NaiveDateTime>)> = sqlx::query_as(
        crate::db::pg("SELECT target_type, target_id, value, created_at FROM votes WHERE user_id = ? ORDER BY id"),
    )
    .bind(user.id)
    .fetch_all(&state.pool)
    .await?;
    let votes_json: Vec<serde_json::Value> = votes
        .into_iter()
        .map(|v| {
            json!({
                "target_type": v.0, "target_id": v.1, "value": v.2, "created_at": v.3,
            })
        })
        .collect();

    // Follows.
    let following: Vec<(String,)> = sqlx::query_as(
        crate::db::pg("SELECT u.username FROM follows f JOIN users u ON u.id = f.followee_id WHERE f.follower_id = ? ORDER BY u.username")
    )
    .bind(user.id).fetch_all(&state.pool).await?;
    let followers: Vec<(String,)> = sqlx::query_as(
        crate::db::pg("SELECT u.username FROM follows f JOIN users u ON u.id = f.follower_id WHERE f.followee_id = ? ORDER BY u.username")
    )
    .bind(user.id).fetch_all(&state.pool).await?;

    // API tokens (hashes only; we never persist plaintext).
    let tokens: Vec<(
        i64,
        Option<String>,
        Option<String>,
        Option<chrono::NaiveDateTime>,
        Option<chrono::NaiveDateTime>,
        Option<chrono::NaiveDateTime>,
    )> = sqlx::query_as(crate::db::pg(
        "SELECT id, token_prefix, name, last_used_at, created_at, expires_at
             FROM api_tokens WHERE user_id = ? ORDER BY id",
    ))
    .bind(user.id)
    .fetch_all(&state.pool)
    .await?;
    let tokens_json: Vec<serde_json::Value> = tokens
        .into_iter()
        .map(|t| {
            json!({
                "id": t.0, "token_prefix": t.1, "name": t.2, "last_used_at": t.3, "created_at": t.4, "expires_at": t.5,
            })
        })
        .collect();

    let bundle = json!({
        "exported_at":   chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        "schema_version": "1",
        "source":        "PreXiv (https://github.com/prexiv/prexiv)",
        "profile":       profile,
        "manuscripts":   ms_json,
        "comments":      cm_json,
        "votes":         votes_json,
        "follows": {
            "following": following.into_iter().map(|(u,)| u).collect::<Vec<_>>(),
            "followers": followers.into_iter().map(|(u,)| u).collect::<Vec<_>>(),
        },
        "api_tokens":    tokens_json,
    });

    let body = serde_json::to_string_pretty(&bundle).unwrap_or_else(|_| "{}".to_string());
    let filename = format!(
        "prexiv-export-{}-{}.json",
        user.username,
        chrono::Utc::now().format("%Y-%m-%d")
    );
    Ok((
        [
            (
                header::CONTENT_TYPE,
                "application/json; charset=utf-8".to_string(),
            ),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            ),
        ],
        body,
    )
        .into_response())
}
