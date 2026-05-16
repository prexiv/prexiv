//! PII-at-rest encryption (S-7).
//!
//! Two primitives operating on a single 32-byte master key loaded from
//! the `PREXIV_DATA_KEY` env var:
//!
//!   * **AES-256-GCM** — confidential storage of values that must round-
//!     trip plaintext (email addresses we still need to display in the
//!     UI and send to). 12-byte random nonce per record, prepended to the
//!     ciphertext + 16-byte tag.
//!
//!   * **HMAC-SHA256** — deterministic 32-byte hash of the lowercased,
//!     trimmed plaintext. Used as a *blind index* so we can do
//!     `WHERE email_hash = ?` lookups (login, "is this email already
//!     registered") without decrypting every row. Constant w.r.t. input,
//!     keyed by the same master key (so an attacker who steals the DB
//!     dump but not the key can't link rows to known emails via a
//!     pre-computed table).
//!
//! ## Key format (env var)
//!
//! `PREXIV_DATA_KEY` is either:
//!   * 64 hex chars  → 32 bytes
//!   * 44 base64 chars (with padding) → 32 bytes (standard alphabet)
//!
//! Anything else fails fast at startup.
//!
//! ## What we encrypt
//!
//! Current uses: account email addresses, pending email-change addresses,
//! TOTP shared secrets, webhook signing secrets, and one-shot session secrets.
//! Passwords and bearer/reset/verification tokens are not encrypted because
//! they should not be recoverable; those are stored as password hashes or
//! one-way token hashes instead.

use aes_gcm::aead::{Aead, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use anyhow::{anyhow, bail, Context, Result};
use base64::Engine as _;
use hmac::Hmac;
use rand::RngCore;
use sha2::Sha256;
use std::sync::OnceLock;

type HmacSha256 = Hmac<Sha256>;

/// 32-byte master key. Set once at startup via [`init`].
static MASTER_KEY: OnceLock<[u8; 32]> = OnceLock::new();

/// Load `PREXIV_DATA_KEY` from the environment and validate it. Call
/// this once during application startup, before any read/write that
/// touches encrypted columns.
pub fn init() -> Result<()> {
    let raw = std::env::var("PREXIV_DATA_KEY")
        .context("PREXIV_DATA_KEY env var must be set (32 bytes, hex or base64)")?;
    let key = decode_key(raw.trim())?;
    MASTER_KEY
        .set(key)
        .map_err(|_| anyhow!("crypto::init called twice"))?;
    Ok(())
}

/// Resolve the active master key. Panics if [`init`] hasn't been called —
/// that's a programmer error, not a runtime condition.
fn key() -> &'static [u8; 32] {
    MASTER_KEY
        .get()
        .expect("crypto::init must be called before any crypto operation")
}

fn decode_key(s: &str) -> Result<[u8; 32]> {
    // Hex first (cheaper to detect, all-hex strings of length 64 are
    // unambiguous). Then base64 (standard alphabet).
    if s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit()) {
        let mut out = [0u8; 32];
        hex::decode_to_slice(s, &mut out).context("PREXIV_DATA_KEY hex decode")?;
        return Ok(out);
    }
    let b = base64::engine::general_purpose::STANDARD
        .decode(s)
        .context("PREXIV_DATA_KEY base64 decode")?;
    if b.len() != 32 {
        bail!(
            "PREXIV_DATA_KEY must decode to 32 bytes (got {} bytes)",
            b.len()
        );
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&b);
    Ok(out)
}

// ─── AES-256-GCM ───────────────────────────────────────────────────────

/// Encrypt arbitrary bytes. Output layout: `nonce (12) || ciphertext || tag (16)`.
/// The same plaintext encrypted twice produces different ciphertext
/// (random nonce); that's the point — it kills cross-row correlation.
pub fn encrypt_blob(plaintext: &[u8]) -> Result<Vec<u8>> {
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key()));
    let mut nonce_bytes = [0u8; 12];
    OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ct = cipher
        .encrypt(nonce, plaintext)
        .map_err(|e| anyhow!("AES-GCM encrypt failed: {e}"))?;
    let mut out = Vec::with_capacity(12 + ct.len());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ct);
    Ok(out)
}

/// Inverse of [`encrypt_blob`]. Returns `Err` if the blob is shorter
/// than 12 + 16 bytes (clearly not ours) or the tag fails to verify
/// (tampered, or encrypted under a different key).
pub fn decrypt_blob(blob: &[u8]) -> Result<Vec<u8>> {
    if blob.len() < 12 + 16 {
        bail!("encrypted blob too short ({} bytes)", blob.len());
    }
    let (nonce_bytes, ct) = blob.split_at(12);
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key()));
    let nonce = Nonce::from_slice(nonce_bytes);
    cipher
        .decrypt(nonce, ct)
        .map_err(|e| anyhow!("AES-GCM decrypt failed: {e}"))
}

// ─── HMAC-SHA256 blind index ───────────────────────────────────────────

/// Deterministic 32-byte HMAC-SHA256 of the lowercased, trimmed
/// plaintext. Two identical emails (modulo case/whitespace) always
/// produce the same hash, which makes it a usable index column.
pub fn email_hash(email: &str) -> [u8; 32] {
    use hmac::Mac as _;
    let norm = normalize_email(email);
    let mut mac =
        <HmacSha256 as hmac::Mac>::new_from_slice(key()).expect("HMAC accepts any key length");
    mac.update(norm.as_bytes());
    let out = mac.finalize().into_bytes();
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&out);
    arr
}

/// Email-shaped values are compared case-insensitively across the
/// industry (RFC 5321 allows providers to be case-sensitive on the
/// local part but in practice every major provider folds case). We
/// trim surrounding whitespace too so a stray copy-paste space doesn't
/// fork the index.
fn normalize_email(s: &str) -> String {
    s.trim().to_ascii_lowercase()
}

// ─── High-level email helpers ──────────────────────────────────────────

/// Encrypt an email address for storage. Returns `(hash, ciphertext)`
/// — the hash goes in `email_hash` (UNIQUE lookup), the ciphertext goes
/// in `email_enc` (so we can recover the original casing/whitespace for
/// display and email delivery).
pub fn seal_email(email: &str) -> Result<([u8; 32], Vec<u8>)> {
    let hash = email_hash(email);
    let enc = encrypt_blob(email.as_bytes())?;
    Ok((hash, enc))
}

/// Decrypt a stored `email_enc` back into the original string.
pub fn open_email(enc: &[u8]) -> Result<String> {
    let bytes = decrypt_blob(enc)?;
    String::from_utf8(bytes).context("email plaintext is not UTF-8")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_test_key<F: FnOnce()>(f: F) {
        // SAFETY: tests run single-threaded by default and don't share
        // MASTER_KEY state across crates. If MASTER_KEY is already set,
        // we reuse it; otherwise install a deterministic dev key.
        let _ = MASTER_KEY.set([7u8; 32]);
        f();
    }

    #[test]
    fn roundtrip() {
        with_test_key(|| {
            let pt = b"alice@example.com";
            let ct = encrypt_blob(pt).unwrap();
            assert_ne!(ct.as_slice(), pt);
            let back = decrypt_blob(&ct).unwrap();
            assert_eq!(back, pt);
        });
    }

    #[test]
    fn ciphertext_unique_per_call() {
        with_test_key(|| {
            let a = encrypt_blob(b"x").unwrap();
            let b = encrypt_blob(b"x").unwrap();
            assert_ne!(a, b, "two encryptions must differ (random nonce)");
        });
    }

    #[test]
    fn hash_normalizes_case_and_whitespace() {
        with_test_key(|| {
            let a = email_hash(" Alice@Example.COM ");
            let b = email_hash("alice@example.com");
            assert_eq!(a, b);
        });
    }
}
