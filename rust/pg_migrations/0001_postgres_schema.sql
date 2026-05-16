-- PreXiv PostgreSQL schema.
--
-- All persistent application data lives in PostgreSQL. User passwords are
-- bcrypt hashes, API/reset/verification tokens are SHA-256 hashes, and email
-- addresses are stored as AES-GCM ciphertext plus a keyed HMAC blind index.

CREATE TABLE IF NOT EXISTS users (
  id                      BIGSERIAL PRIMARY KEY,
  username                TEXT UNIQUE NOT NULL,
  -- Legacy compatibility column. New writes keep this empty; email_enc is the
  -- source of truth for user email display and delivery.
  email                   TEXT NOT NULL DEFAULT '',
  password_hash           TEXT NOT NULL,
  display_name            TEXT,
  affiliation             TEXT,
  bio                     TEXT,
  karma                   BIGINT DEFAULT 0,
  is_admin                BIGINT NOT NULL DEFAULT 0,
  email_verified          BIGINT NOT NULL DEFAULT 0,
  email_verify_token      TEXT,
  email_verify_expires    BIGINT,
  password_reset_token    TEXT,
  password_reset_expires  BIGINT,
  totp_secret             TEXT,
  totp_enabled            BIGINT NOT NULL DEFAULT 0,
  orcid                   TEXT,
  created_at              TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
  email_hash              BYTEA,
  email_enc               BYTEA,
  orcid_verified          BIGINT NOT NULL DEFAULT 0,
  institutional_email     BIGINT NOT NULL DEFAULT 0,
  orcid_oauth_verified    BIGINT NOT NULL DEFAULT 0,
  orcid_oauth_verified_at TIMESTAMP,
  orcid_oauth_sub         TEXT
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_users_email_hash
  ON users(email_hash)
  WHERE email_hash IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS idx_users_orcid_oauth_sub_unique
  ON users(orcid_oauth_sub)
  WHERE orcid_oauth_sub IS NOT NULL AND orcid_oauth_sub <> '';

CREATE TABLE IF NOT EXISTS manuscripts (
  id                        BIGSERIAL PRIMARY KEY,
  arxiv_like_id             TEXT UNIQUE,
  doi                       TEXT UNIQUE,
  submitter_id              BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  title                     TEXT NOT NULL,
  abstract                  TEXT NOT NULL,
  authors                   TEXT NOT NULL,
  category                  TEXT NOT NULL,
  secondary_categories      TEXT,
  pdf_path                  TEXT,
  source_path               TEXT,
  pdf_text                  TEXT,
  external_url              TEXT,
  conductor_type            TEXT NOT NULL DEFAULT 'human-ai'
                            CHECK (conductor_type IN ('human-ai', 'ai-agent')),
  conductor_ai_model        TEXT NOT NULL,
  conductor_ai_model_public BIGINT NOT NULL DEFAULT 1,
  conductor_human           TEXT,
  conductor_human_public    BIGINT NOT NULL DEFAULT 1,
  conductor_role            TEXT,
  conductor_notes           TEXT,
  agent_framework           TEXT,
  has_auditor               BIGINT NOT NULL DEFAULT 0,
  auditor_name              TEXT,
  auditor_affiliation       TEXT,
  auditor_role              TEXT,
  auditor_statement         TEXT,
  auditor_orcid             TEXT,
  view_count                BIGINT DEFAULT 0,
  score                     BIGINT DEFAULT 0,
  comment_count             BIGINT DEFAULT 0,
  withdrawn                 BIGINT NOT NULL DEFAULT 0,
  withdrawn_reason          TEXT,
  withdrawn_at              TIMESTAMP,
  created_at                TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at                TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
  license                   TEXT NOT NULL DEFAULT 'CC-BY-4.0',
  ai_training               TEXT NOT NULL DEFAULT 'allow',
  current_version           BIGINT NOT NULL DEFAULT 1,
  search_vector             TSVECTOR GENERATED ALWAYS AS (
    setweight(to_tsvector('english', coalesce(title, '')), 'A') ||
    setweight(to_tsvector('english', coalesce(abstract, '')), 'B') ||
    setweight(to_tsvector('english', coalesce(authors, '')), 'C') ||
    setweight(to_tsvector('english', coalesce(pdf_text, '')), 'D')
  ) STORED
);

CREATE INDEX IF NOT EXISTS idx_manuscripts_created ON manuscripts(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_manuscripts_score ON manuscripts(score DESC);
CREATE INDEX IF NOT EXISTS idx_manuscripts_cat ON manuscripts(category);
CREATE INDEX IF NOT EXISTS idx_manuscripts_license ON manuscripts(license);
CREATE INDEX IF NOT EXISTS idx_manuscripts_ai_training ON manuscripts(ai_training);
CREATE INDEX IF NOT EXISTS idx_manuscripts_search ON manuscripts USING GIN(search_vector);

CREATE TABLE IF NOT EXISTS comments (
  id            BIGSERIAL PRIMARY KEY,
  manuscript_id BIGINT NOT NULL REFERENCES manuscripts(id) ON DELETE CASCADE,
  author_id     BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  parent_id     BIGINT REFERENCES comments(id) ON DELETE CASCADE,
  content       TEXT NOT NULL,
  score         BIGINT DEFAULT 0,
  created_at    TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX IF NOT EXISTS idx_comments_manuscript ON comments(manuscript_id);
CREATE INDEX IF NOT EXISTS idx_comments_parent ON comments(parent_id);

CREATE TABLE IF NOT EXISTS votes (
  id          BIGSERIAL PRIMARY KEY,
  user_id     BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  target_type TEXT NOT NULL CHECK(target_type IN ('manuscript','comment')),
  target_id   BIGINT NOT NULL,
  value       BIGINT NOT NULL CHECK(value IN (-1, 1)),
  created_at  TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE(user_id, target_type, target_id)
);
CREATE INDEX IF NOT EXISTS idx_votes_target ON votes(target_type, target_id);

CREATE TABLE IF NOT EXISTS audit_endorsements (
  id            BIGSERIAL PRIMARY KEY,
  manuscript_id BIGINT NOT NULL REFERENCES manuscripts(id) ON DELETE CASCADE,
  user_id       BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  statement     TEXT NOT NULL,
  created_at    TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE(manuscript_id, user_id)
);

CREATE TABLE IF NOT EXISTS flag_reports (
  id              BIGSERIAL PRIMARY KEY,
  target_type     TEXT NOT NULL CHECK(target_type IN ('manuscript','comment')),
  target_id       BIGINT NOT NULL,
  reporter_id     BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  reason          TEXT NOT NULL,
  resolved        BIGINT NOT NULL DEFAULT 0,
  resolved_by_id  BIGINT REFERENCES users(id) ON DELETE SET NULL,
  resolved_at     TIMESTAMP,
  resolution_note TEXT,
  created_at      TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE(target_type, target_id, reporter_id)
);
CREATE INDEX IF NOT EXISTS idx_flags_unresolved ON flag_reports(resolved, created_at DESC);

CREATE TABLE IF NOT EXISTS api_tokens (
  id           BIGSERIAL PRIMARY KEY,
  user_id      BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  token_hash   TEXT NOT NULL UNIQUE,
  name         TEXT,
  last_used_at TIMESTAMP,
  created_at   TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
  expires_at   TIMESTAMP
);
CREATE INDEX IF NOT EXISTS idx_api_tokens_user ON api_tokens(user_id);

CREATE TABLE IF NOT EXISTS manuscript_versions (
  id              BIGSERIAL PRIMARY KEY,
  manuscript_id   BIGINT NOT NULL REFERENCES manuscripts(id) ON DELETE CASCADE,
  version_number  BIGINT NOT NULL,
  title           TEXT NOT NULL,
  abstract        TEXT NOT NULL,
  authors         TEXT NOT NULL,
  category        TEXT NOT NULL,
  pdf_path        TEXT,
  external_url    TEXT,
  conductor_notes TEXT,
  license         TEXT,
  ai_training     TEXT,
  revision_note   TEXT,
  revised_at      TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE(manuscript_id, version_number)
);
CREATE INDEX IF NOT EXISTS idx_mv_manuscript ON manuscript_versions(manuscript_id);

CREATE TABLE IF NOT EXISTS audit_log (
  id            BIGSERIAL PRIMARY KEY,
  actor_user_id BIGINT REFERENCES users(id) ON DELETE SET NULL,
  action        TEXT NOT NULL,
  target_type   TEXT,
  target_id     BIGINT,
  detail        TEXT,
  ip            TEXT,
  created_at    TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX IF NOT EXISTS idx_audit_log_created ON audit_log(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_audit_log_actor ON audit_log(actor_user_id);

CREATE TABLE IF NOT EXISTS notifications (
  id           BIGSERIAL PRIMARY KEY,
  recipient_id BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  actor_id     BIGINT REFERENCES users(id) ON DELETE SET NULL,
  kind         TEXT NOT NULL,
  target_type  TEXT,
  target_id    BIGINT,
  detail       TEXT,
  read_at      TIMESTAMP,
  created_at   TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX IF NOT EXISTS idx_notif_recipient_unread ON notifications(recipient_id, read_at);
CREATE INDEX IF NOT EXISTS idx_notif_recipient_created ON notifications(recipient_id, created_at DESC);

CREATE TABLE IF NOT EXISTS follows (
  follower_id BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  followee_id BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  created_at  TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (follower_id, followee_id),
  CHECK (follower_id != followee_id)
);
CREATE INDEX IF NOT EXISTS idx_follows_followee ON follows(followee_id);

CREATE TABLE IF NOT EXISTS webhooks (
  id              BIGSERIAL PRIMARY KEY,
  user_id         BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  url             TEXT NOT NULL,
  secret          TEXT NOT NULL,
  events          TEXT NOT NULL,
  active          BIGINT NOT NULL DEFAULT 1,
  description     TEXT,
  failure_count   BIGINT NOT NULL DEFAULT 0,
  last_attempt_at TIMESTAMP,
  last_status     BIGINT,
  created_at      TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX IF NOT EXISTS idx_webhooks_user ON webhooks(user_id);

CREATE TABLE IF NOT EXISTS email_verification_tokens (
  id         BIGSERIAL PRIMARY KEY,
  user_id    BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  token_hash TEXT NOT NULL UNIQUE,
  created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
  expires_at TIMESTAMP NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_evt_user ON email_verification_tokens(user_id);

CREATE TABLE IF NOT EXISTS password_reset_tokens (
  id         BIGSERIAL PRIMARY KEY,
  user_id    BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  token_hash TEXT NOT NULL UNIQUE,
  created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
  expires_at TIMESTAMP NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_prt_user ON password_reset_tokens(user_id);

CREATE TABLE IF NOT EXISTS pending_email_changes (
  id         BIGSERIAL PRIMARY KEY,
  user_id    BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  -- Legacy compatibility column. New writes keep this empty; the pending
  -- address is stored encrypted in new_email_enc.
  new_email  TEXT NOT NULL DEFAULT '',
  new_email_hash BYTEA NOT NULL,
  new_email_enc  BYTEA NOT NULL,
  token_hash TEXT NOT NULL UNIQUE,
  created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
  expires_at TIMESTAMP NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_pec_user ON pending_email_changes(user_id);

CREATE TABLE IF NOT EXISTS user_totp (
  user_id    BIGINT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
  secret     BYTEA NOT NULL,
  enabled_at TIMESTAMP,
  created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS prexiv_id_aliases (
  old_slug   TEXT PRIMARY KEY,
  new_slug   TEXT NOT NULL,
  aliased_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX IF NOT EXISTS idx_prexiv_id_aliases_new ON prexiv_id_aliases(new_slug);
