#![allow(clippy::type_complexity)]
//! JSON REST API under /api/v1 — the agent-ready path.
//!
//! Read endpoints are public. Public write endpoints and token creation
//! require a Bearer token from a GitHub-, ORCID-, or email-verified account. All
//! responses are JSON; errors come back as `{ "error": "...", "details"?: ... }`
//! with the appropriate status.

use std::path::{Path as FsPath, PathBuf};

use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::fs;
use tokio::io::AsyncWriteExt;

use crate::api_auth::{generate_token, hash_token, token_display_prefix, ApiUser, ApiVerifiedUser};
use crate::models::{Manuscript, ManuscriptListItem};
use crate::state::AppState;

const JSON_ARTIFACT_BODY_LIMIT: usize = 45 * 1024 * 1024;

// ─── JSON-native error type for /api/v1 ───────────────────────────────
//
// AppError renders HTML by default; for /api/* we must answer in JSON so
// machine clients can parse it. ApiError carries the same kinds but its
// IntoResponse emits `{"error": "..."}` with the right status. Sqlx /
// anyhow conversions match AppError so existing `?`-propagating code in
// this module only needs a type-alias swap (`AppResult` -> `ApiResult`).

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("not found")]
    NotFound,
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub type ApiResult<T> = Result<T, ApiError>;

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, body) = match self {
            ApiError::NotFound => (StatusCode::NOT_FOUND, json!({ "error": "not found" })),
            ApiError::Sqlx(sqlx::Error::RowNotFound) => {
                (StatusCode::NOT_FOUND, json!({ "error": "not found" }))
            }
            ApiError::Sqlx(e) => {
                tracing::error!(error = %e, "sqlx error in api handler");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    json!({ "error": "internal error" }),
                )
            }
            ApiError::Other(e) => {
                tracing::error!(error = %e, "api handler error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    json!({ "error": "internal error" }),
                )
            }
        };
        (status, Json(body)).into_response()
    }
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/me", get(get_me))
        .route("/me/tokens", get(list_tokens).post(create_token))
        .route("/me/tokens/{id}", delete(revoke_token))
        .route("/categories", get(get_categories))
        .route(
            "/manuscripts",
            get(list_manuscripts)
                .post(post_manuscript)
                .layer(DefaultBodyLimit::max(JSON_ARTIFACT_BODY_LIMIT)),
        )
        .route("/manuscripts/{id}", get(get_manuscript))
        .route(
            "/manuscripts/{id}/comments",
            get(list_comments).post(post_comment),
        )
        .route("/manuscripts/{id}/vote", post(vote_manuscript))
        .route(
            "/manuscripts/{id}/versions",
            get(list_manuscript_versions).post(post_manuscript_version),
        )
        .route(
            "/manuscripts/{id}/versions/{n}",
            get(get_manuscript_version),
        )
        .route("/search", get(search))
        .route("/openapi.json", get(openapi))
        .route("/manifest", get(manifest))
}

// ─── /me ───────────────────────────────────────────────────────────────────

async fn get_me(ApiUser(u): ApiUser) -> Json<Value> {
    Json(json!({
        "id": u.id, "username": u.username, "email": u.email,
        "display_name": u.display_name, "affiliation": u.affiliation,
        "bio": u.bio, "karma": u.karma.unwrap_or(0),
        "is_admin": u.is_admin(),
        "account_verified": u.is_account_verified(),
        "email_verified": u.is_verified(),
        "github_oauth_verified": u.is_github_oauth_verified(),
        "github_login": if u.is_github_oauth_verified() { u.github_login.clone() } else { None },
        "orcid": if u.is_orcid_oauth_verified() { u.orcid.clone() } else { None },
        "orcid_oauth_verified": u.is_orcid_oauth_verified(),
        "created_at": u.created_at,
    }))
}

// ─── /me/tokens ────────────────────────────────────────────────────────────

async fn list_tokens(State(state): State<AppState>, ApiUser(u): ApiUser) -> ApiResult<Json<Value>> {
    let rows: Vec<(i64, Option<String>, Option<String>, Option<chrono::NaiveDateTime>, Option<chrono::NaiveDateTime>, Option<chrono::NaiveDateTime>)> =
        sqlx::query_as(crate::db::pg("SELECT id, token_prefix, name, last_used_at, created_at, expires_at FROM api_tokens WHERE user_id = ? ORDER BY created_at DESC"))
            .bind(u.id)
            .fetch_all(&state.pool)
            .await?;
    let items: Vec<Value> = rows
        .into_iter()
        .map(|(id, token_prefix, name, last_used_at, created_at, expires_at)| {
            json!({"id": id, "token_prefix": token_prefix, "name": name, "last_used_at": last_used_at, "created_at": created_at, "expires_at": expires_at})
        })
        .collect();
    Ok(Json(json!({"items": items})))
}

#[derive(Deserialize)]
pub struct CreateTokenBody {
    #[serde(default)]
    pub name: Option<String>,
    /// Days until expiry. None = never expires.
    #[serde(default)]
    pub expires_in_days: Option<i64>,
}

async fn create_token(
    State(state): State<AppState>,
    ApiVerifiedUser(u): ApiVerifiedUser,
    Json(body): Json<CreateTokenBody>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let plain = generate_token();
    let hash = hash_token(&plain);
    let prefix = token_display_prefix(&plain);
    let expires_at: Option<chrono::NaiveDateTime> = body
        .expires_in_days
        .filter(|d| *d > 0)
        .map(|d| (chrono::Utc::now() + chrono::Duration::days(d)).naive_utc());

    let (token_id,): (i64,) = sqlx::query_as(
        crate::db::pg("INSERT INTO api_tokens (user_id, token_hash, token_prefix, name, expires_at) VALUES (?, ?, ?, ?, ?) RETURNING id"),
    )
    .bind(u.id)
    .bind(&hash)
    .bind(&prefix)
    .bind(body.name.as_deref())
    .bind(expires_at)
    .fetch_one(&state.pool)
    .await?;
    let _ = sqlx::query(crate::db::pg(
        "INSERT INTO audit_log (actor_user_id, action, target_type, target_id, detail) VALUES (?, 'api_token_mint', 'api_token', ?, ?)",
    ))
    .bind(u.id)
    .bind(token_id)
    .bind(serde_json::json!({
        "token_prefix": prefix.as_str(),
        "name": body.name.as_deref(),
        "expires_at": expires_at,
        "surface": "api"
    }).to_string())
    .execute(&state.pool)
    .await;

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": token_id,
            "token_prefix": prefix,
            "name": body.name,
            "token": plain,
            "warning": "Save this token now — it will never be shown again. Treat it like a password.",
            "expires_at": expires_at,
        })),
    ))
}

async fn revoke_token(
    State(state): State<AppState>,
    ApiUser(u): ApiUser,
    Path(id): Path<i64>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let deleted: Option<(Option<String>, Option<String>)> = sqlx::query_as(crate::db::pg(
        "DELETE FROM api_tokens WHERE id = ? AND user_id = ? RETURNING token_prefix, name",
    ))
    .bind(id)
    .bind(u.id)
    .fetch_optional(&state.pool)
    .await?;
    if deleted.is_none() {
        return Ok((
            StatusCode::NOT_FOUND,
            Json(json!({"ok": false, "error": "no such token"})),
        ));
    }
    if let Some((token_prefix, name)) = deleted {
        let _ = sqlx::query(crate::db::pg(
            "INSERT INTO audit_log (actor_user_id, action, target_type, target_id, detail) VALUES (?, 'api_token_revoke', 'api_token', ?, ?)",
        ))
        .bind(u.id)
        .bind(id)
        .bind(serde_json::json!({
            "token_prefix": token_prefix,
            "name": name,
            "surface": "api"
        }).to_string())
        .execute(&state.pool)
        .await;
    }
    Ok((StatusCode::OK, Json(json!({"ok": true, "deleted_id": id}))))
}

// ─── /categories ───────────────────────────────────────────────────────────

async fn get_categories() -> Json<Value> {
    let arr: Vec<Value> = crate::categories::CATEGORIES
        .iter()
        .map(|c| json!({"id": c.id, "name": c.name, "group": c.group}))
        .collect();
    Json(json!(arr))
}

// ─── /manuscripts ──────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub page: Option<i64>,
    #[serde(default)]
    pub per: Option<i64>,
}

async fn list_manuscripts(
    State(state): State<AppState>,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    let per = q.per.unwrap_or(30).clamp(1, 100);
    let page = q.page.unwrap_or(1).max(1);
    let offset = (page - 1) * per;

    let base = "SELECT id, arxiv_like_id, doi, title, authors, category,
                conductor_type, conductor_ai_model, conductor_ai_model_public,
                conductor_human, conductor_human_public,
                has_auditor, auditor_name,
                score, comment_count, withdrawn, created_at
                FROM manuscripts";

    let (where_clause, order, bind_cat) =
        match (q.category.as_deref(), q.mode.as_deref().unwrap_or("ranked")) {
            (Some(_), _) => ("WHERE category = ?", "ORDER BY created_at DESC", true),
            (None, "new") => ("", "ORDER BY created_at DESC", false),
            (None, "top") => ("", "ORDER BY score DESC, created_at DESC", false),
            (None, "audited") => ("WHERE has_auditor = 1", "ORDER BY created_at DESC", false),
            (None, _) => ("", "ORDER BY score DESC, created_at DESC", false),
        };
    let sql = crate::db::pg_dynamic(&format!("{base} {where_clause} {order} LIMIT ? OFFSET ?"));
    let mut query = sqlx::query_as::<_, ManuscriptListItem>(&sql);
    if bind_cat {
        query = query.bind(q.category.as_deref().unwrap_or(""));
    }
    let items: Vec<ManuscriptListItem> =
        query.bind(per).bind(offset).fetch_all(&state.pool).await?;

    Ok(Json(json!({
        "items": items.iter().map(redact_list_item).collect::<Vec<_>>(),
        "page": page, "per": per,
        "mode": q.mode.unwrap_or_else(|| "ranked".into()),
        "category": q.category,
    })))
}

async fn get_manuscript(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let m: Option<Manuscript> = sqlx::query_as::<_, Manuscript>(crate::db::pg(
        r#"SELECT id, arxiv_like_id, doi, submitter_id, title, abstract, authors, category,
                  pdf_path, external_url, source_path,
                  conductor_type, conductor_ai_model, conductor_ai_model_public,
                  conductor_human, conductor_human_public, conductor_role, conductor_notes,
                  agent_framework,
                  has_auditor, auditor_name, auditor_affiliation, auditor_role,
                  auditor_statement, auditor_orcid,
                  view_count, score, comment_count,
                  withdrawn, withdrawn_reason, withdrawn_at,
                  created_at, updated_at
           FROM manuscripts
           WHERE arxiv_like_id = ? OR CAST(id AS TEXT) = ?
           LIMIT 1"#,
    ))
    .bind(&id)
    .bind(&id)
    .fetch_optional(&state.pool)
    .await?;
    let m = m.ok_or(ApiError::NotFound)?;
    let _ = sqlx::query(crate::db::pg(
        "UPDATE manuscripts SET view_count = COALESCE(view_count, 0) + 1 WHERE id = ?",
    ))
    .bind(m.id)
    .execute(&state.pool)
    .await;
    Ok(Json(redact_manuscript(&m)))
}

/// Body for POST /api/v1/manuscripts — JSON only. Agents upload the
/// hosted artifact by base64-encoding either LaTeX source or a finished
/// PDF; `external_url` is only a supplemental link.
#[derive(Deserialize, Serialize, Debug, Default)]
#[serde(default)]
pub struct ManuscriptIn {
    pub title: String,
    pub r#abstract: String,
    pub authors: String,
    pub category: String,
    pub external_url: Option<String>,
    /// Base64-encoded finished PDF. Mutually exclusive with
    /// `source_base64`; direct PDF uploads cannot be used when private
    /// conductor/model fields require source redaction.
    pub pdf_base64: Option<String>,
    pub pdf_filename: Option<String>,
    /// Base64-encoded `.tex`, `.zip`, `.tar.gz`, or `.tgz` source. The
    /// server prepares/redacts it, compiles it, watermarks the PDF, and
    /// stores only the public artifact.
    pub source_base64: Option<String>,
    pub source_filename: Option<String>,

    pub conductor_type: Option<String>, // "human-ai" (default) or "ai-agent"
    /// Single-model legacy form. Either this OR `conductor_ai_models`
    /// must be present. If both are given, `conductor_ai_models` wins.
    #[serde(default)]
    pub conductor_ai_model: String,
    /// Preferred shape: array of one-or-more model identifiers, e.g.
    /// `["Claude Opus 4.7", "GPT-5.5 Pro"]`.
    #[serde(default)]
    pub conductor_ai_models: Vec<String>,
    pub conductor_ai_model_public: Option<bool>, // default true
    pub conductor_human: Option<String>,
    pub conductor_human_public: Option<bool>, // default true
    pub conductor_role: Option<String>,
    pub conductor_notes: Option<String>,
    pub agent_framework: Option<String>,

    pub has_auditor: Option<bool>,
    pub auditor_name: Option<String>,
    pub auditor_affiliation: Option<String>,
    pub auditor_role: Option<String>,
    pub auditor_statement: Option<String>,
    pub auditor_orcid: Option<String>,

    pub license: Option<String>,
    pub ai_training: Option<String>,
}

async fn post_manuscript(
    State(state): State<AppState>,
    ApiVerifiedUser(user): ApiVerifiedUser,
    Json(v): Json<ManuscriptIn>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let mut errors = vec![];
    if v.title.trim().is_empty() {
        errors.push("title is required");
    }
    if v.r#abstract.trim().len() < 100 {
        errors.push("abstract must be at least 100 chars");
    }
    if v.authors.trim().is_empty() {
        errors.push("authors is required");
    }
    if v.category.trim().is_empty() {
        errors.push("category is required");
    }
    // Normalize multi-AI input: `conductor_ai_models: [...]` wins; if
    // missing we fall back to splitting the legacy `conductor_ai_model`
    // string on commas. Either path must produce ≥1 non-empty model.
    let ai_models_joined: String = if !v.conductor_ai_models.is_empty() {
        crate::models::manuscript::normalize_ai_models(&v.conductor_ai_models.join(", "))
    } else {
        crate::models::manuscript::normalize_ai_models(&v.conductor_ai_model)
    };
    if ai_models_joined.is_empty() {
        errors.push("conductor_ai_models is required (at least one model name)");
    }
    let conductor_type = v.conductor_type.as_deref().unwrap_or("human-ai");
    if !matches!(conductor_type, "human-ai" | "ai-agent") {
        errors.push("conductor_type must be 'human-ai' or 'ai-agent'");
    }
    if conductor_type == "human-ai" && v.conductor_human.as_deref().unwrap_or("").trim().is_empty()
    {
        errors.push("conductor_human is required when conductor_type='human-ai'");
    }
    let has_pdf_payload = v
        .pdf_base64
        .as_deref()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    let has_source_payload = v
        .source_base64
        .as_deref()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    if has_pdf_payload == has_source_payload {
        errors.push("provide exactly one hosted artifact: source_base64 or pdf_base64");
    }
    if !errors.is_empty() {
        return Ok((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "error": "validation failed",
                "details": errors,
            })),
        ));
    }

    let model_public = v.conductor_ai_model_public.unwrap_or(true);
    let human_public = v.conductor_human_public.unwrap_or(true);
    let has_auditor = v.has_auditor.unwrap_or(false);
    if has_pdf_payload && (!model_public || (conductor_type == "human-ai" && !human_public)) {
        return Ok((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "error": "validation failed",
                "details": ["private conductor/model fields require source_base64 so PreXiv can black out the public source and compiled PDF; direct PDF uploads cannot be automatically redacted"]
            })),
        ));
    }
    let license = v
        .license
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("CC-BY-4.0");
    if crate::licenses::lookup(license).is_none() {
        return Ok((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "error": "validation failed",
                "details": ["unknown reader license"]
            })),
        ));
    }
    let ai_training = v
        .ai_training
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("allow");
    if crate::licenses::ai_training_lookup(ai_training).is_none() {
        return Ok((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "error": "validation failed",
                "details": ["unknown AI-training policy"]
            })),
        ));
    }

    // Allocate id + INSERT with retry on UNIQUE collision (see submit.rs
    // for rationale).
    let mut new_id: i64 = 0;
    let mut ok = false;
    let mut last_err: Option<sqlx::Error> = None;
    for _ in 0..3 {
        let arxiv_like_id = make_prexiv_id_for_api();
        let synthetic_doi = format!("10.99999/{}", arxiv_like_id);
        let artifact = match persist_api_artifact(
            &state,
            &v,
            &arxiv_like_id,
            &ai_models_joined,
            conductor_type,
            model_public,
            human_public,
        )
        .await
        {
            Ok(a) => a,
            Err(msg) => {
                return Ok((
                    StatusCode::UNPROCESSABLE_ENTITY,
                    Json(json!({
                        "error": "validation failed",
                        "details": [msg],
                    })),
                ));
            }
        };
        let mut tx = state.pool.begin().await?;
        let r = sqlx::query_as::<_, (i64,)>(crate::db::pg(
            r#"INSERT INTO manuscripts (
                arxiv_like_id, doi, submitter_id, title, abstract, authors, category,
                pdf_path, external_url, source_path,
                conductor_type, conductor_ai_model, conductor_ai_model_public,
                conductor_human, conductor_human_public, conductor_role, conductor_notes,
                agent_framework,
                has_auditor, auditor_name, auditor_affiliation, auditor_role,
                auditor_statement, auditor_orcid,
                license, ai_training,
                score
            ) VALUES (
                ?, ?, ?, ?, ?, ?, ?,
                ?, ?, ?,
                ?, ?, ?,
                ?, ?, ?, ?,
                ?,
                ?, ?, ?, ?,
                ?, ?,
                ?, ?,
                1
            )
            RETURNING id"#,
        ))
        .bind(&arxiv_like_id)
        .bind(&synthetic_doi)
        .bind(user.id)
        .bind(v.title.trim())
        .bind(v.r#abstract.trim())
        .bind(v.authors.trim())
        .bind(v.category.trim())
        .bind(artifact.pdf_path.as_deref())
        .bind(
            v.external_url
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty()),
        )
        .bind(artifact.source_path.as_deref())
        .bind(conductor_type)
        .bind(&ai_models_joined)
        .bind(if model_public { 1i64 } else { 0 })
        .bind(if conductor_type == "human-ai" {
            v.conductor_human.as_deref()
        } else {
            None
        })
        .bind(if human_public { 1i64 } else { 0 })
        .bind(if conductor_type == "human-ai" {
            v.conductor_role.as_deref()
        } else {
            None
        })
        .bind(v.conductor_notes.as_deref())
        .bind(if conductor_type == "ai-agent" {
            v.agent_framework.as_deref()
        } else {
            None
        })
        .bind(if has_auditor { 1i64 } else { 0 })
        .bind(if has_auditor {
            v.auditor_name.as_deref()
        } else {
            None
        })
        .bind(if has_auditor {
            v.auditor_affiliation.as_deref()
        } else {
            None
        })
        .bind(if has_auditor {
            v.auditor_role.as_deref()
        } else {
            None
        })
        .bind(if has_auditor {
            v.auditor_statement.as_deref()
        } else {
            None
        })
        .bind(if has_auditor {
            v.auditor_orcid.as_deref()
        } else {
            None
        })
        .bind(license)
        .bind(ai_training)
        .fetch_one(&mut *tx)
        .await;
        match r {
            Ok(rr) => {
                new_id = rr.0;
            }
            Err(e) if api_is_unique_violation(&e) => {
                cleanup_api_uploads(
                    &api_upload_dir(),
                    artifact.pdf_path.as_deref(),
                    artifact.source_path.as_deref(),
                )
                .await;
                last_err = Some(e);
                continue;
            }
            Err(e) => {
                cleanup_api_uploads(
                    &api_upload_dir(),
                    artifact.pdf_path.as_deref(),
                    artifact.source_path.as_deref(),
                )
                .await;
                return Err(e.into());
            }
        }
        // Self-upvote (matches the JS app).
        let _ = sqlx::query(crate::db::pg("INSERT INTO votes (user_id, target_type, target_id, value) VALUES (?, 'manuscript', ?, 1)"))
            .bind(user.id)
            .bind(new_id)
            .execute(&mut *tx)
            .await;

        // Initial v1 in manuscript_versions, inside the same transaction.
        let title_t = v.title.trim();
        let abstract_t = v.r#abstract.trim();
        let authors_t = v.authors.trim();
        let category_t = v.category.trim();
        let ext_url = v
            .external_url
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let cond_notes = v
            .conductor_notes
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let _ = crate::versions::insert_initial(
            &mut *tx,
            new_id,
            &crate::versions::VersionInput {
                title: title_t,
                r#abstract: abstract_t,
                authors: authors_t,
                category: category_t,
                pdf_path: artifact.pdf_path.as_deref(),
                external_url: ext_url,
                conductor_notes: cond_notes,
                license: "CC-BY-4.0",
                ai_training: "allow",
                revision_note: None,
            },
        )
        .await;

        tx.commit().await?;
        ok = true;
        break;
    }
    if !ok {
        return Err(ApiError::Other(anyhow::anyhow!(
            "could not allocate a unique prexiv id after retries: {}",
            last_err
                .map(|e| e.to_string())
                .unwrap_or_else(|| "unknown".to_string())
        )));
    }

    // Fetch and return the freshly-created row.
    let m: Manuscript = sqlx::query_as::<_, Manuscript>(crate::db::pg(
        r#"SELECT id, arxiv_like_id, doi, submitter_id, title, abstract, authors, category,
                  pdf_path, external_url, source_path,
                  conductor_type, conductor_ai_model, conductor_ai_model_public,
                  conductor_human, conductor_human_public, conductor_role, conductor_notes,
                  agent_framework,
                  has_auditor, auditor_name, auditor_affiliation, auditor_role,
                  auditor_statement, auditor_orcid,
                  view_count, score, comment_count,
                  withdrawn, withdrawn_reason, withdrawn_at,
                  created_at, updated_at,
                  license, ai_training, current_version
           FROM manuscripts WHERE id = ?"#,
    ))
    .bind(new_id)
    .fetch_one(&state.pool)
    .await?;

    Ok((StatusCode::CREATED, Json(redact_manuscript(&m))))
}

struct ApiArtifact {
    pdf_path: Option<String>,
    source_path: Option<String>,
}

async fn persist_api_artifact(
    state: &AppState,
    v: &ManuscriptIn,
    arxiv_like_id: &str,
    ai_models_joined: &str,
    conductor_type: &str,
    model_public: bool,
    human_public: bool,
) -> Result<ApiArtifact, String> {
    let upload_dir = api_upload_dir();
    fs::create_dir_all(&upload_dir)
        .await
        .map_err(|e| format!("could not prepare upload directory: {e}"))?;
    let stamp = chrono::Utc::now().timestamp_millis();
    let rnd: u32 = rand::thread_rng().gen_range(100_000..1_000_000);
    let app_url = state.app_url.as_deref().unwrap_or("http://localhost:3001");

    if let Some(raw_pdf) = v.pdf_base64.as_deref().filter(|s| !s.trim().is_empty()) {
        let filename = sanitize_api_filename(v.pdf_filename.as_deref().unwrap_or("upload.pdf"));
        if !filename.to_ascii_lowercase().ends_with(".pdf") {
            return Err("pdf_filename must end with .pdf".to_string());
        }
        let data = decode_api_base64("pdf_base64", raw_pdf)?;
        if data.len() > 30 * 1024 * 1024 {
            return Err("PDF exceeds 30 MB.".to_string());
        }
        if !data.starts_with(b"%PDF-") {
            return Err("Uploaded PDF is not valid (missing %PDF header).".to_string());
        }
        let watermarked =
            crate::pdf_watermark::watermark_pdf(&data, arxiv_like_id, v.category.trim(), app_url)
                .await
                .map_err(|e| format!("PDF watermarking failed: {e}"))?;
        let stored = format!("{stamp}-{rnd}-{filename}");
        let full = upload_dir.join(&stored);
        let mut f = fs::File::create(&full)
            .await
            .map_err(|e| format!("could not store PDF: {e}"))?;
        f.write_all(&watermarked)
            .await
            .map_err(|e| format!("could not store PDF: {e}"))?;
        return Ok(ApiArtifact {
            pdf_path: Some(stored),
            source_path: None,
        });
    }

    let Some(raw_source) = v.source_base64.as_deref().filter(|s| !s.trim().is_empty()) else {
        return Err("source_base64 or pdf_base64 is required.".to_string());
    };
    let filename = sanitize_api_filename(v.source_filename.as_deref().unwrap_or("source.tex"));
    let data = decode_api_base64("source_base64", raw_source)?;
    if data.len() > 30 * 1024 * 1024 {
        return Err("Source upload exceeds 30 MB.".to_string());
    }
    let redaction = crate::compile::RedactionOptions {
        hide_human: conductor_type == "human-ai" && !human_public,
        hide_ai_model: !model_public,
        human_name: v.conductor_human.as_deref().map(str::to_string),
        ai_models: ai_models_joined
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
    };
    let prepared = crate::compile::prepare_source(&filename, &data, &redaction)
        .map_err(|e| format!("LaTeX source preparation failed: {e}"))?;

    let stored_src = format!(
        "{stamp}-{rnd}-src-{}",
        sanitize_api_filename(&prepared.filename)
    );
    let full_src = upload_dir.join(&stored_src);
    let mut f = fs::File::create(&full_src)
        .await
        .map_err(|e| format!("could not store source: {e}"))?;
    f.write_all(&prepared.data)
        .await
        .map_err(|e| format!("could not store source: {e}"))?;

    let compiled = match crate::compile::compile(&prepared.filename, &prepared.data).await {
        Ok(compiled) => compiled,
        Err(e) => {
            let _ = fs::remove_file(upload_dir.join(&stored_src)).await;
            let log_excerpt = e.log().unwrap_or("");
            let msg = if log_excerpt.is_empty() {
                format!("LaTeX compile failed: {e}")
            } else {
                format!(
                    "LaTeX compile failed: {e}\n\nLast lines of the compile log:\n\n{log_excerpt}"
                )
            };
            return Err(msg);
        }
    };
    let watermarked = match crate::pdf_watermark::watermark_pdf(
        &compiled.pdf,
        arxiv_like_id,
        v.category.trim(),
        app_url,
    )
    .await
    {
        Ok(pdf) => pdf,
        Err(e) => {
            let _ = fs::remove_file(upload_dir.join(&stored_src)).await;
            return Err(format!("PDF watermarking failed: {e}"));
        }
    };
    let pdf_name = format!("{stamp}-{rnd}-compiled.pdf");
    let pdf_full = upload_dir.join(&pdf_name);
    let mut pf = fs::File::create(&pdf_full)
        .await
        .map_err(|e| format!("could not store compiled PDF: {e}"))?;
    pf.write_all(&watermarked)
        .await
        .map_err(|e| format!("could not store compiled PDF: {e}"))?;

    Ok(ApiArtifact {
        pdf_path: Some(pdf_name),
        source_path: Some(stored_src),
    })
}

fn decode_api_base64(label: &str, raw: &str) -> Result<Vec<u8>, String> {
    let trimmed = raw.trim();
    let payload = if let Some((prefix, data)) = trimmed.split_once(',') {
        if prefix.to_ascii_lowercase().contains("base64") {
            data
        } else {
            trimmed
        }
    } else {
        trimmed
    };
    let compact: String = payload.chars().filter(|c| !c.is_whitespace()).collect();
    BASE64_STANDARD
        .decode(compact.as_bytes())
        .map_err(|e| format!("{label} is not valid base64: {e}"))
}

fn api_upload_dir() -> PathBuf {
    std::env::var_os("UPLOAD_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .map(|p| p.join("public").join("uploads"))
                .unwrap_or_else(|| PathBuf::from("./public/uploads"))
        })
}

fn sanitize_api_filename(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if s.trim_matches('.').is_empty() {
        s = "upload".to_string();
    }
    if s.len() > 80 {
        s.chars().take(80).collect()
    } else {
        s
    }
}

async fn cleanup_api_uploads(
    upload_dir: &FsPath,
    pdf_path: Option<&str>,
    source_path: Option<&str>,
) {
    if let Some(path) = pdf_path {
        let _ = fs::remove_file(upload_dir.join(path)).await;
    }
    if let Some(path) = source_path {
        let _ = fs::remove_file(upload_dir.join(path)).await;
    }
}

// ─── /manuscripts/{id}/comments ────────────────────────────────────────────

async fn list_comments(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let m: Option<(i64,)> = sqlx::query_as::<_, (i64,)>(crate::db::pg(
        "SELECT id FROM manuscripts WHERE arxiv_like_id = ? OR CAST(id AS TEXT) = ? LIMIT 1",
    ))
    .bind(&id)
    .bind(&id)
    .fetch_optional(&state.pool)
    .await?;
    let manuscript_id = m.ok_or(ApiError::NotFound)?.0;
    let rows: Vec<(
        i64,
        i64,
        String,
        Option<i64>,
        String,
        Option<i64>,
        Option<chrono::NaiveDateTime>,
    )> = sqlx::query_as(crate::db::pg(
        "SELECT c.id, c.author_id, u.username, c.parent_id, c.content, c.score, c.created_at
             FROM comments c JOIN users u ON u.id = c.author_id
             WHERE c.manuscript_id = ? ORDER BY c.created_at ASC",
    ))
    .bind(manuscript_id)
    .fetch_all(&state.pool)
    .await?;
    let items: Vec<Value> = rows.into_iter().map(|(cid, author_id, username, parent_id, content, score, created_at)| {
        json!({"id": cid, "author_id": author_id, "author_username": username, "parent_id": parent_id, "content": content, "score": score, "created_at": created_at})
    }).collect();
    Ok(Json(json!({"items": items})))
}

#[derive(Deserialize)]
pub struct CommentIn {
    pub content: String,
    #[serde(default)]
    pub parent_id: Option<i64>,
}

async fn post_comment(
    State(state): State<AppState>,
    ApiVerifiedUser(user): ApiVerifiedUser,
    Path(id): Path<String>,
    Json(body): Json<CommentIn>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let content = body.content.trim();
    if content.is_empty() || content.len() > 8000 {
        return Ok((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"error": "content must be 1..=8000 chars"})),
        ));
    }
    let m: Option<(i64, i64)> = sqlx::query_as::<_, (i64, i64)>(
        crate::db::pg("SELECT id, withdrawn FROM manuscripts WHERE arxiv_like_id = ? OR CAST(id AS TEXT) = ? LIMIT 1")
    )
    .bind(&id).bind(&id)
    .fetch_optional(&state.pool).await?;
    let (manuscript_id, withdrawn) = m.ok_or(ApiError::NotFound)?;
    if withdrawn != 0 {
        return Ok((
            StatusCode::CONFLICT,
            Json(json!({"error": "manuscript is withdrawn; comments are disabled"})),
        ));
    }

    let mut tx = state.pool.begin().await?;
    let (cid,): (i64,) = sqlx::query_as(
        crate::db::pg("INSERT INTO comments (manuscript_id, author_id, parent_id, content) VALUES (?, ?, ?, ?) RETURNING id"),
    )
    .bind(manuscript_id)
    .bind(user.id)
    .bind(body.parent_id)
    .bind(content)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query(crate::db::pg(
        "UPDATE manuscripts SET comment_count = COALESCE(comment_count, 0) + 1 WHERE id = ?",
    ))
    .bind(manuscript_id)
    .execute(&mut *tx)
    .await?;
    let submitter: Option<(i64,)> = sqlx::query_as(crate::db::pg(
        "SELECT submitter_id FROM manuscripts WHERE id = ?",
    ))
    .bind(manuscript_id)
    .fetch_optional(&mut *tx)
    .await?;
    let parent_author: Option<(i64,)> = match body.parent_id {
        Some(pid) => {
            sqlx::query_as(crate::db::pg("SELECT author_id FROM comments WHERE id = ?"))
                .bind(pid)
                .fetch_optional(&mut *tx)
                .await?
        }
        None => None,
    };
    tx.commit().await?;

    let snippet: String = content.chars().take(140).collect();
    if let Some((sid,)) = submitter {
        let _ = crate::notifications::notify(
            &state.pool,
            sid,
            Some(user.id),
            crate::notifications::KIND_COMMENT_ON_MY_MANUSCRIPT,
            Some("comment"),
            Some(cid),
            Some(&snippet),
        )
        .await;
    }
    if let Some((pid_author,)) = parent_author {
        let _ = crate::notifications::notify(
            &state.pool,
            pid_author,
            Some(user.id),
            crate::notifications::KIND_REPLY_TO_MY_COMMENT,
            Some("comment"),
            Some(cid),
            Some(&snippet),
        )
        .await;
    }
    Ok((StatusCode::CREATED, Json(json!({"id": cid}))))
}

// ─── /manuscripts/{id}/vote ────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct VoteBody {
    pub value: i64,
}

async fn vote_manuscript(
    State(state): State<AppState>,
    ApiVerifiedUser(user): ApiVerifiedUser,
    Path(id): Path<String>,
    Json(body): Json<VoteBody>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    if !matches!(body.value, -1 | 1) {
        return Ok((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"error": "value must be -1 or 1"})),
        ));
    }
    let m: Option<(i64, i64)> = sqlx::query_as::<_, (i64, i64)>(
        crate::db::pg("SELECT id, withdrawn FROM manuscripts WHERE arxiv_like_id = ? OR CAST(id AS TEXT) = ? LIMIT 1")
    )
    .bind(&id).bind(&id)
    .fetch_optional(&state.pool).await?;
    let (target_id, withdrawn) = m.ok_or(ApiError::NotFound)?;
    if withdrawn != 0 {
        return Ok((
            StatusCode::CONFLICT,
            Json(json!({"error": "manuscript is withdrawn; voting is disabled"})),
        ));
    }

    let mut tx = state.pool.begin().await?;
    sqlx::query(crate::db::pg(
        "INSERT INTO votes (user_id, target_type, target_id, value) VALUES (?, 'manuscript', ?, ?)
         ON CONFLICT(user_id, target_type, target_id) DO UPDATE SET value = excluded.value",
    ))
    .bind(user.id)
    .bind(target_id)
    .bind(body.value)
    .execute(&mut *tx)
    .await?;
    let (score,): (i64,) = sqlx::query_as(
        crate::db::pg("SELECT COALESCE(SUM(value), 0) FROM votes WHERE target_type = 'manuscript' AND target_id = ?")
    )
    .bind(target_id)
    .fetch_one(&mut *tx).await?;
    sqlx::query(crate::db::pg(
        "UPDATE manuscripts SET score = ? WHERE id = ?",
    ))
    .bind(score)
    .bind(target_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok((StatusCode::OK, Json(json!({"ok": true, "score": score}))))
}

// ─── /manuscripts/{id}/versions ───────────────────────────────────────────

async fn list_manuscript_versions(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let m: Option<(i64, i64)> = sqlx::query_as::<_, (i64, i64)>(
        crate::db::pg("SELECT id, current_version FROM manuscripts WHERE arxiv_like_id = ? OR CAST(id AS TEXT) = ? LIMIT 1")
    )
    .bind(&id).bind(&id)
    .fetch_optional(&state.pool).await?;
    let (manuscript_id, current_version) = m.ok_or(ApiError::NotFound)?;
    let vs = crate::versions::list_versions(&state.pool, manuscript_id).await?;
    let items: Vec<Value> = vs
        .iter()
        .map(|v| {
            json!({
                "version_number": v.version_number,
                "revision_note":  v.revision_note,
                "revised_at":     v.revised_at,
                "title":          v.title,
                "authors":        v.authors,
                "category":       v.category,
                "pdf_path":       v.pdf_path,
                "external_url":   v.external_url,
                "license":        v.license,
                "ai_training":    v.ai_training,
                "is_current":     v.version_number == current_version,
            })
        })
        .collect();
    Ok(Json(json!({
        "items": items,
        "current_version": current_version,
    })))
}

async fn get_manuscript_version(
    State(state): State<AppState>,
    Path((id, n)): Path<(String, i64)>,
) -> ApiResult<Json<Value>> {
    let m: Option<(i64,)> = sqlx::query_as::<_, (i64,)>(crate::db::pg(
        "SELECT id FROM manuscripts WHERE arxiv_like_id = ? OR CAST(id AS TEXT) = ? LIMIT 1",
    ))
    .bind(&id)
    .bind(&id)
    .fetch_optional(&state.pool)
    .await?;
    let manuscript_id = m.ok_or(ApiError::NotFound)?.0;
    let v = crate::versions::get_version(&state.pool, manuscript_id, n)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(json!({
        "version_number": v.version_number,
        "revision_note":  v.revision_note,
        "revised_at":     v.revised_at,
        "title":          v.title,
        "abstract":       v.r#abstract,
        "authors":        v.authors,
        "category":       v.category,
        "pdf_path":       v.pdf_path,
        "external_url":   v.external_url,
        "conductor_notes": v.conductor_notes,
        "license":        v.license,
        "ai_training":    v.ai_training,
    })))
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct RevisionIn {
    pub title: String,
    pub r#abstract: String,
    pub authors: String,
    pub category: String,
    pub external_url: Option<String>,
    pub conductor_notes: Option<String>,
    pub license: Option<String>,
    pub ai_training: Option<String>,
    /// Required; describes what changed.
    pub revision_note: String,
}

async fn post_manuscript_version(
    State(state): State<AppState>,
    ApiVerifiedUser(user): ApiVerifiedUser,
    Path(id): Path<String>,
    Json(v): Json<RevisionIn>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let m: Option<(
        i64,
        i64,
        i64,
        Option<String>,
        Option<String>,
        Option<String>,
    )> = sqlx::query_as(crate::db::pg(
        "SELECT id, submitter_id, withdrawn, pdf_path, license, ai_training
             FROM manuscripts WHERE arxiv_like_id = ? OR CAST(id AS TEXT) = ? LIMIT 1",
    ))
    .bind(&id)
    .bind(&id)
    .fetch_optional(&state.pool)
    .await?;
    let (manuscript_id, submitter_id, withdrawn, pdf_path, existing_license, existing_ai) =
        m.ok_or(ApiError::NotFound)?;

    if submitter_id != user.id && !user.is_admin() {
        return Ok((
            StatusCode::FORBIDDEN,
            Json(json!({
                "error": "only the submitter or an admin may revise a manuscript"
            })),
        ));
    }
    if withdrawn != 0 {
        return Ok((
            StatusCode::CONFLICT,
            Json(json!({
                "error": "manuscript is withdrawn; revisions are disabled"
            })),
        ));
    }

    // Validation.
    let mut errors: Vec<&str> = vec![];
    if v.title.trim().is_empty() {
        errors.push("title is required");
    }
    if v.r#abstract.trim().len() < 100 {
        errors.push("abstract must be at least 100 chars");
    }
    if v.authors.trim().is_empty() {
        errors.push("authors is required");
    }
    if v.category.trim().is_empty() {
        errors.push("category is required");
    }
    if v.revision_note.trim().is_empty() {
        errors.push("revision_note is required \u{2014} a short summary of what changed");
    }
    if !errors.is_empty() {
        return Ok((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "error": "validation failed",
                "details": errors,
            })),
        ));
    }

    // The API can't accept a new PDF in JSON. Inherit the previous one.
    let license_resolved = v
        .license
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or(existing_license.as_deref())
        .unwrap_or("CC-BY-4.0");
    let ai_resolved = v
        .ai_training
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or(existing_ai.as_deref())
        .unwrap_or("allow");

    let new_version = crate::versions::mint_revision(
        &state.pool,
        manuscript_id,
        &crate::versions::VersionInput {
            title: v.title.trim(),
            r#abstract: v.r#abstract.trim(),
            authors: v.authors.trim(),
            category: v.category.trim(),
            pdf_path: pdf_path.as_deref(),
            external_url: v
                .external_url
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty()),
            conductor_notes: v
                .conductor_notes
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty()),
            license: license_resolved,
            ai_training: ai_resolved,
            revision_note: Some(v.revision_note.trim()),
        },
    )
    .await?;

    let _ = sqlx::query(
        crate::db::pg("INSERT INTO audit_log (actor_user_id, action, target_type, target_id, detail) VALUES (?, 'manuscript_revise', 'manuscript', ?, ?)"),
    )
    .bind(user.id)
    .bind(manuscript_id)
    .bind(format!("v{new_version}: {} (via API)", v.revision_note.trim()))
    .execute(&state.pool)
    .await;

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "ok": true,
            "version_number": new_version,
            "manuscript_id": manuscript_id,
        })),
    ))
}

// ─── /search ───────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct SearchQuery {
    #[serde(default)]
    pub q: String,
}

async fn search(
    State(state): State<AppState>,
    Query(p): Query<SearchQuery>,
) -> ApiResult<Json<Value>> {
    let q = p.q.trim();
    if q.is_empty() {
        return Ok(Json(json!({"items": [], "q": ""})));
    }
    let fts: String = q
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| format!("{t}:*"))
        .collect::<Vec<_>>()
        .join(" & ");
    if fts.is_empty() {
        return Ok(Json(json!({"items": [], "q": q})));
    }
    let rows: Vec<ManuscriptListItem> = sqlx::query_as::<_, ManuscriptListItem>(crate::db::pg(
        r#"SELECT m.id, m.arxiv_like_id, m.doi, m.title, m.authors, m.category,
                  m.conductor_type, m.conductor_ai_model, m.conductor_ai_model_public,
                  m.conductor_human, m.conductor_human_public,
                  m.has_auditor, m.auditor_name,
                  m.score, m.comment_count, m.withdrawn, m.created_at
           FROM manuscripts m, (SELECT to_tsquery('english', ?) AS q) query
           WHERE m.search_vector @@ query.q
           ORDER BY ts_rank(m.search_vector, query.q) DESC, m.created_at DESC
           LIMIT 50"#,
    ))
    .bind(&fts)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(json!({
        "items": rows.iter().map(redact_list_item).collect::<Vec<_>>(),
        "q": q,
    })))
}

// ─── /openapi.json + /manifest ─────────────────────────────────────────────

async fn openapi() -> Json<Value> {
    Json(openapi_spec())
}

async fn manifest() -> Json<Value> {
    Json(json!({
        "name": "PreXiv",
        "tagline": "agent-ready preprint server",
        "version": "v1",
        "api_base": "/api/v1",
        "auth": {
            "type": "bearer",
            "header": "Authorization: Bearer prexiv_…",
            "mint_url": "/me/tokens",
            "scopes": "all (single-scope tokens for now)",
            "verified_required_for_public_writes": true
        },
        "id_format": "prexiv:YYMMDD.xxxxxx",
        "doi_format_synthetic": "10.99999/<id>",
        "endpoints": {
            "whoami":           "GET  /api/v1/me",
            "list_tokens":      "GET  /api/v1/me/tokens",
            "create_token":     "POST /api/v1/me/tokens  body: {name?, expires_in_days?}",
            "revoke_token":     "DELETE /api/v1/me/tokens/{id}",
            "list_manuscripts": "GET  /api/v1/manuscripts?mode=new|top|audited|ranked&category=…&page=…&per=…",
            "read_manuscript":  "GET  /api/v1/manuscripts/{id}",
            "submit":           "POST /api/v1/manuscripts  (JSON; include exactly one of source_base64 or pdf_base64; external_url optional)",
            "search":           "GET  /api/v1/search?q=…",
            "list_comments":    "GET  /api/v1/manuscripts/{id}/comments",
            "post_comment":     "POST /api/v1/manuscripts/{id}/comments",
            "vote":             "POST /api/v1/manuscripts/{id}/vote  body: {value: 1|-1}",
            "categories":       "GET  /api/v1/categories",
            "openapi":          "GET  /api/v1/openapi.json",
        },
        "agent_contract": [
            "Public writes and token creation require a valid bearer token owned by an account verified through GitHub OAuth, ORCID OAuth, or email; public reads and token revocation do not.",
            "Be honest about conductor_type ('human-ai' or 'ai-agent').",
            "Set conductor_ai_model to the actual model identifier.",
            "If autonomous (ai-agent), disclose that no human conductor directed production; the token owner remains responsible for lawful posting and accurate provenance.",
            "Do not list a human auditor who has not actually read the manuscript and signed a scoped public audit statement.",
            "Manuscripts can be searched, voted, commented on, and cited; treat the corpus accordingly."
        ]
    }))
}

fn openapi_spec() -> Value {
    json!({
        "openapi": "3.1.0",
        "info": {
            "title": "PreXiv API",
            "version": "1.0.0",
            "description": "Agent-native research manuscript archive. Bearer-token auth from a GitHub-, ORCID-, or email-verified account on public write endpoints. Mint a token at /me/tokens after account verification.",
        },
        "servers": [{"url": "/api/v1"}],
        "components": {
            "securitySchemes": {
                "bearer": {"type": "http", "scheme": "bearer", "bearerFormat": "prexiv_…"}
            }
        },
        "paths": {
            "/me": {"get": {"summary": "Whoami", "security": [{"bearer": []}], "responses": {"200": {"description": "current user"}}}},
            "/me/tokens": {
                "get":  {"summary": "List your tokens", "security": [{"bearer": []}], "responses": {"200": {"description": "ok"}}},
                "post": {"summary": "Mint a new token", "security": [{"bearer": []}], "responses": {"201": {"description": "created — plaintext shown once"}}}
            },
            "/me/tokens/{id}": {
                "delete": {"summary": "Revoke a token", "security": [{"bearer": []}], "parameters": [{"name":"id","in":"path","required":true,"schema":{"type":"integer"}}], "responses": {"200": {"description": "ok"}}}
            },
            "/manuscripts": {
                "get":  {"summary": "List manuscripts", "responses": {"200": {"description": "ok"}}},
                "post": {
                    "summary": "Submit a manuscript",
                    "description": "JSON submission requires exactly one hosted artifact: source_base64 (+ source_filename) or pdf_base64 (+ pdf_filename). external_url is optional and supplemental.",
                    "security": [{"bearer": []}],
                    "responses": {"201": {"description": "created"}, "422": {"description": "validation failed"}}
                }
            },
            "/manuscripts/{id}": {"get":  {"summary": "Read manuscript", "responses": {"200": {"description": "ok"}, "404": {"description": "not found"}}}},
            "/manuscripts/{id}/comments": {
                "get":  {"summary": "List comments", "responses": {"200": {"description": "ok"}}},
                "post": {"summary": "Post a comment", "security": [{"bearer": []}], "responses": {"201": {"description": "created"}}}
            },
            "/manuscripts/{id}/vote": {"post": {"summary": "Up/down-vote", "security": [{"bearer": []}], "responses": {"200": {"description": "ok"}}}},
            "/search": {"get": {"summary": "Full-text search", "responses": {"200": {"description": "ok"}}}},
            "/categories": {"get": {"summary": "Category list", "responses": {"200": {"description": "ok"}}}},
            "/openapi.json": {"get": {"summary": "This document", "responses": {"200": {"description": "ok"}}}},
            "/manifest": {"get": {"summary": "Human-readable agent manifest", "responses": {"200": {"description": "ok"}}}}
        }
    })
}

// ─── redaction ─────────────────────────────────────────────────────────────

fn redact_list_item(m: &ManuscriptListItem) -> Value {
    let public = m.conductor_ai_model_public != 0;
    let ai_models: Vec<String> = if public {
        m.ai_models().into_iter().map(String::from).collect()
    } else {
        vec!["(undisclosed)".to_string()]
    };
    let ai_joined = if public {
        m.conductor_ai_model.clone()
    } else {
        "(undisclosed)".to_string()
    };
    let human = if m.conductor_human_public != 0 {
        m.conductor_human.clone()
    } else {
        Some("(undisclosed)".to_string())
    };
    json!({
        "id": m.id, "arxiv_like_id": m.arxiv_like_id, "doi": m.doi,
        "title": m.title, "authors": m.authors, "category": m.category,
        "conductor_type": m.conductor_type,
        "conductor_ai_model":  ai_joined,   // legacy comma-joined string
        "conductor_ai_models": ai_models,   // preferred array shape
        "conductor_human": human,
        "score": m.score.unwrap_or(0),
        "comment_count": m.comment_count.unwrap_or(0),
        "withdrawn": m.withdrawn != 0,
        "created_at": m.created_at,
    })
}

fn redact_manuscript(m: &Manuscript) -> Value {
    let public = m.conductor_ai_model_public != 0;
    let ai_models: Vec<String> = if public {
        m.ai_models().into_iter().map(String::from).collect()
    } else {
        vec!["(undisclosed)".to_string()]
    };
    let ai_joined = if public {
        m.conductor_ai_model.clone()
    } else {
        "(undisclosed)".to_string()
    };
    let human = if m.conductor_human_public != 0 {
        m.conductor_human.clone()
    } else {
        Some("(undisclosed)".to_string())
    };
    json!({
        "id": m.id, "arxiv_like_id": m.arxiv_like_id, "doi": m.doi,
        "submitter_id": m.submitter_id,
        "title": m.title, "abstract": m.r#abstract, "authors": m.authors, "category": m.category,
        "pdf_path": m.pdf_path, "external_url": m.external_url,
        "source_path": m.source_path,
        "conductor_type": m.conductor_type,
        "conductor_ai_model":  ai_joined,
        "conductor_ai_models": ai_models,
        "conductor_human": human,
        "conductor_role": m.conductor_role,
        "conductor_notes": m.conductor_notes,
        "agent_framework": m.agent_framework,
        "has_auditor": m.has_auditor != 0,
        "auditor_name": m.auditor_name,
        "auditor_affiliation": m.auditor_affiliation,
        "auditor_role": m.auditor_role,
        "auditor_statement": m.auditor_statement,
        "auditor_orcid": m.auditor_orcid,
        "view_count": m.view_count.unwrap_or(0),
        "score": m.score.unwrap_or(0),
        "comment_count": m.comment_count.unwrap_or(0),
        "withdrawn": m.withdrawn != 0,
        "withdrawn_reason": m.withdrawn_reason,
        "withdrawn_at": m.withdrawn_at,
        "created_at": m.created_at,
        "updated_at": m.updated_at,
    })
}

/// API-side allocator. Same algorithm as `submit::make_prexiv_id` —
/// `prexiv:YYMMDD.xxxxxx` with a random Crockford-32 suffix. The
/// UNIQUE constraint on `arxiv_like_id` plus the caller's small retry
/// loop handles collisions, which are vanishingly rare given the
/// ~10^9 suffix space.
fn make_prexiv_id_for_api() -> String {
    use chrono::Datelike;
    use rand::Rng;
    let now = chrono::Utc::now();
    let yymmdd = format!("{:02}{:02}{:02}", now.year() % 100, now.month(), now.day());
    let suffix_n: u32 = rand::thread_rng().gen_range(0..(1u32 << 30));
    format!(
        "prexiv:{yymmdd}.{}",
        crate::crockford::encode(suffix_n as u64, 6)
    )
}

fn api_is_unique_violation(e: &sqlx::Error) -> bool {
    if let sqlx::Error::Database(db) = e {
        if db.code().as_deref() == Some("23505") {
            return true;
        }
        let m = db.message().to_ascii_lowercase();
        return m.contains("unique constraint") || m.contains("constraint failed");
    }
    false
}
