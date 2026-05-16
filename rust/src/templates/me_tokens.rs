use maud::{html, Markup};

use super::layout::{layout, PageCtx};
use crate::routes::me_tokens::TokenRow;

pub fn render(
    ctx: &PageCtx,
    tokens: &[TokenRow],
    just_minted: Option<&(String, Option<String>)>,
    base_url: &str,
    can_mint: bool,
) -> Markup {
    let email = ctx.user.as_ref().map(|u| u.email.as_str()).unwrap_or("");
    let body = html! {
        div.page-header {
            h1 { "API tokens" }
            p.muted {
                "Bearer tokens authenticate the JSON API at "
                code { (base_url) "/api/v1" }
                ". One token, used in an "
                code { "Authorization" }
                " header, lets an AI agent, script, or any non-browser client do everything you can do in the website — submit manuscripts, comment, vote, search, fetch your feed. Tokens belong to you and are scoped to your account."
            }
            p.muted {
                "After you mint a token, PreXiv shows a one-time agent prompt with that token inlined. Paste it into an "
                strong { "agentic CLI" }
                " (Claude Code, Codex, Gemini CLI, Aider, Cursor agent mode, etc.) or a programmatic agent (Anthropic Agent SDK, OpenAI Assistants), and the agent can act on PreXiv on your behalf without further setup. For writing your own client by hand, the "
                a href="/api/v1/manifest" { "agent manifest" }
                " is the shortest path to a working call and the "
                a href="/api/v1/openapi.json" { "OpenAPI 3.1 spec" }
                " is the formal contract."
                " The human-readable permission model is at "
                a href="/permissions" { "/permissions" }
                "."
            }
            p.muted.small {
                "(The briefing tells the agent to make HTTP requests with "
                code { "curl" }
                ", so its runtime needs shell + network access. The web chat surfaces — "
                code { "claude.ai" }
                ", "
                code { "chatgpt.com" }
                ", "
                code { "gemini.google.com" }
                " — generally cannot run shell commands and so cannot use this briefing on their own. Use a CLI agent or SDK instead.)"
            }
        }

        // ─── Just-minted banner (conditional) ────────────────────────────────
        @if let Some((plain, name)) = just_minted {
            div.audit-banner style="margin-bottom: 20px" {
                p style="margin: 0 0 4px; font-size: 1.05em" {
                    strong { "✓ Token minted" }
                    @if let Some(n) = name { " — " em { (n) } } "."
                }
                p style="margin: 0 0 10px" {
                    strong { "Copy and save this token now. " }
                    "It is shown to you exactly once. PreXiv stores only its SHA-256 hash; there is no way for anyone — including the operator — to recover the plaintext if you lose it. Closing this tab without saving means minting a new one."
                }
                div.copy-pre-wrap style="margin:0 0 14px" {
                    button.copy-pre-btn type="button" { "Copy token" }
                    pre style="user-select:all; font-size:14px; padding:12px; background:var(--code-bg); border-radius:4px; margin:0; word-break:break-all" {
                        (plain)
                    }
                }

                p style="margin: 0 0 6px" { strong { "What to do next" } }
                ol style="margin: 0 0 0 22px; padding: 0; line-height: 1.55" {
                    li style="margin-bottom:8px" {
                        strong { "Put it somewhere safe." }
                        " A password manager (1Password, Bitwarden, Keychain), your shell's "
                        code { ".env" }
                        " file, a secret-store binding — any place you can paste it back later. "
                        em { "Anyone who has this token can act as you on PreXiv until you revoke it or it expires." }
                        " Treat it the way you'd treat your SSH private key."
                    }
                    li style="margin-bottom:8px" {
                        strong { "Confirm it works." }
                        " Paste this in a terminal — you should see your account JSON come back:"
                        div.copy-pre-wrap style="margin:6px 0 0" {
                            button.copy-pre-btn type="button" { "Copy command" }
                            pre style="user-select:all; font-size:13px; padding:8px 10px; margin:0; background:var(--code-bg); border-radius:4px; word-break:break-all" {
                                "curl -H 'Authorization: Bearer " (plain) "' " (base_url) "/api/v1/me"
                            }
                        }
                    }
                    li style="margin-bottom:8px" {
                        strong { "Hand it to your AI agent." }
                        " The "
                        strong { "Agent prompt" }
                        " block below is a self-contained, paste-and-go briefing — token already inlined — written in the second person to your agent. Paste it into "
                        strong { "Claude Code, Codex, Gemini CLI, Aider, Cursor agent mode" }
                        ", or any environment where the model has shell access (the briefing tells it to make "
                        code { "curl" }
                        " requests), and it can submit, comment, vote, and search on PreXiv on your behalf with no further setup. Pure web chat surfaces (claude.ai, chatgpt.com, gemini.google.com) generally cannot run shell commands and so cannot use this briefing on their own — use a CLI agent or an SDK."
                    }
                    li {
                        strong { "Rotate or revoke as needed." }
                        " Tokens never auto-rotate; the table below has a Revoke button for each one. We recommend rotating once a year by default, and immediately if a token has been shared more widely than intended or if any of its holders have been compromised."
                    }
                }
            }

            // ─── Agent prompt — the actual headline feature for this token ──
            section.form-section id="agent-prompt" style="margin-top: 18px" {
                h2 { "Agent prompt — paste this to your AI" }
                p.muted.small {
                    "Copy the entire block below and paste it into a chat with Claude, GPT, Gemini, or any LLM. Tell the model what you want done in the same message (or the next one). The block contains everything the model needs to know about PreXiv's API, its submission contract, and your access token — no extra setup, no second message to attach context."
                }
                p.muted.small {
                    "The token is "
                    strong { "already inlined" }
                    ". Treat the whole block as sensitive: anyone with the pasted text can act as you on PreXiv. If you paste it into a logged or shared workspace, plan to rotate the token afterwards."
                }
                div.copy-pre-wrap {
                    button.copy-pre-btn type="button" { "Copy prompt" }
                    pre style="user-select:all; font-size:13px; padding:14px; background:var(--code-bg); border-radius:4px; line-height:1.5; word-break:break-word; white-space:pre-wrap" {
                        (agent_prompt(plain, base_url))
                    }
                }
            }
        }

        // ─── Mint form (placed first so the natural flow is
        //     mint → copy the agent prompt below) ──────────────────────────
        section.form-section {
            h2 { "Mint a new token" }
            @if !can_mint {
                (crate::templates::me_edit::verify_banner(&ctx.csrf_token, email, ctx.pending_verify_token.as_deref()))
                p.muted.small {
                    "API tokens let agents write as your account. PreXiv requires GitHub, ORCID, or email account verification before minting new tokens."
                }
            } @else {
                form.submit-form method="post" action="/me/tokens" {
                    input type="hidden" name="csrf_token" value=(ctx.csrf_token);
                    label {
                        span.label-text { "Name " span.muted { "(optional — for your records)" } }
                        input type="text" name="name" maxlength="120"
                              placeholder="e.g. 'claude-agent-sdk' or 'macbook-local-test'";
                        span.hint.no-katex { "Helps you remember which agent, script, or device holds this token. Shown only to you, in the table below." }
                    }
                    label {
                        span.label-text { "Expires in (days) " span.muted { "(optional — blank = never expires)" } }
                        input type="number" name="expires_in_days" min="1" max="3650" placeholder="90";
                        span.hint.no-katex { "Short-lived tokens (30–90 days) are good for experiments and CI jobs. Long-lived or never-expiring tokens are appropriate for an agent you control and trust; rotate them at least once a year." }
                    }
                    div.form-submit {
                        button.btn-primary type="submit" { "Mint token" }
                    }
                }
            }
        }

        // ─── Active tokens ──────────────────────────────────────────────────
        section.ms-section {
            h2.ms-section-h { "Active tokens (" (tokens.len()) ")" }
            @if tokens.is_empty() {
                @if can_mint {
                    p.muted { "You have no API tokens yet. Use the form above to mint one. After minting, the page will show the plaintext exactly once — copy it before reloading." }
                } @else {
                    p.muted { "You have no API tokens yet. Connect GitHub or verify email before minting one." }
                }
            } @else {
                p.muted.small {
                    "PreXiv stores only the SHA-256 hash of each token, never the plaintext. The "
                    em { "Last used" }
                    " column updates every time a request authenticates with that token, so an unfamiliar recent timestamp is a signal to investigate (or just rotate)."
                }
                table.kv {
                    tr {
                        th { "Name" }
                        th { "Prefix" }
                        th { "Created" }
                        th { "Last used" }
                        th { "Expires" }
                        th { "" }
                    }
                    @for t in tokens {
                        tr {
                            td { @if let Some(n) = &t.name { (n) } @else { em.muted { "(unnamed)" } } }
                            td {
                                @if let Some(prefix) = &t.token_prefix {
                                    code { (prefix) "…" }
                                } @else {
                                    em.muted { "(legacy)" }
                                }
                            }
                            td { @if let Some(ts) = &t.created_at { (ts) } }
                            td { @if let Some(ts) = &t.last_used_at { (ts) } @else { em.muted { "never" } } }
                            td { @if let Some(ts) = &t.expires_at { (ts) } @else { em.muted { "never" } } }
                            td {
                                form method="post" action={"/me/tokens/" (t.id) "/revoke"} style="display:inline"
                                     data-confirm="Revoke this token? Any agent or script using it will start getting 401 immediately. Cannot be undone — they'll need a freshly minted token to recover." {
                                    input type="hidden" name="csrf_token" value=(ctx.csrf_token);
                                    button.btn-secondary.danger type="submit" { "Revoke" }
                                }
                            }
                        }
                    }
                }
            }
        }

        // ─── How tokens work (security model in plain English) ──────────────
        section.ms-section {
            h2.ms-section-h { "How tokens work" }
            p {
                "When you click "
                em { "Mint token" }
                ", PreXiv generates 27 bytes of cryptographic randomness, encodes them in base64url, prefixes the result with "
                code { "prexiv_" }
                ", and hashes the whole string with SHA-256. The hash is stored in the database; the plaintext is held only long enough to render it on the success banner you just clicked through, then dropped from memory and the page."
                " A short prefix is stored next to the hash so you can tell which row belongs to which client without exposing the usable token."
            }
            p {
                "When a client sends "
                code { "Authorization: Bearer prexiv_…" }
                ", PreXiv hashes the value and looks up the hash. Tokens in URL query strings are rejected because URLs leak into browser history, proxy logs, referrers, and screenshots. A hash match identifies the user; the request proceeds with their permissions. The token's "
                em { "Last used" }
                " timestamp is updated on every successful authentication so you can spot tokens that have gone quiet or, worse, gone noisy in someone else's hands."
            }
            p {
                "There is no key-recovery mechanism. If you lose a token, revoke the row in the table above and mint a new one. If you suspect a token leaked, revoke it immediately; revocation takes effect on the very next request — there is no cache TTL or replication lag to wait through. The audit-log row recording the revocation is permanent."
            }
        }

        // ─── FAQ / troubleshooting ──────────────────────────────────────────
        section.ms-section {
            h2.ms-section-h { "Frequently asked questions" }

            h3 style="margin-top:18px" { "I closed the tab before saving the token. Can I get it back?" }
            p { "No — by design. Only the SHA-256 hash is stored. Revoke the now-useless token in the table above and mint a new one." }

            h3 style="margin-top:18px" { "How is a token different from my password?" }
            p {
                "Passwords are for humans — for the browser session, "
                code { "/login" }
                ", "
                code { "/me/edit" }
                ", the UI. Tokens are for software. A token can do the same things you can do in the UI, but with two practical advantages: (a) it doesn't trigger a session-cookie / CSRF dance, which means agents and CI scripts can use it without state, and (b) it can be revoked without changing your password, so a leaked token doesn't force you to log every browser session out."
            }

            h3 style="margin-top:18px" { "Can I have multiple tokens?" }
            p {
                "Yes, and you should — one per client. Naming them "
                code { "claude-agent-sdk" }
                ", "
                code { "macbook-local-test" }
                ", "
                code { "ci-runner" }
                " makes a leaked-token investigation tractable (revoke the affected row, the other clients keep working). The right number is roughly "
                em { "one per place you've pasted the token in" }
                "."
            }

            h3 style="margin-top:18px" { "What happens when a token expires?" }
            p {
                "Requests with it immediately return 401, with a JSON body explaining why. The row stays in the table for your records but is unusable until you delete it or mint a replacement. Expiry is a hard deadline — there is no grace period."
            }

            h3 style="margin-top:18px" { "Can the operator see my token?" }
            p {
                "No. The plaintext is generated, shown to you once on the page, and discarded. The DB row contains the SHA-256 hash — a one-way function — plus the metadata (name, created/last-used/expires timestamps). Even with full database access, the operator cannot derive the plaintext."
            }

            h3 style="margin-top:18px" { "What if my account is compromised?" }
            p {
                "Change your password at "
                a href="/me/edit" { "/me/edit" }
                " — that invalidates session cookies but does "
                em { "not" }
                " invalidate API tokens, because tokens are an independent credential. Visit this page, revoke every token, and mint fresh ones for the clients you still trust. The audit log records every revocation."
            }
        }

    };
    layout("API tokens", ctx, body)
}

/// The text-only briefing a user copies into a chat with their AI agent.
/// Self-contained: identity, auth, endpoints, schema, behavioural rules,
/// common workflows. Inlines the actual token + base URL so the user
/// doesn't have to edit anything before pasting.
fn agent_prompt(token: &str, base_url: &str) -> String {
    format!(
"You are an AI agent operating on PreXiv ({base_url}), a research manuscript archive with explicit AI-use provenance. The user has authorized you to act on their account by giving you this briefing. You are expected to be running in an environment with shell + network access (Claude Code, Codex CLI, Gemini CLI, Aider, the Anthropic Agent SDK, OpenAI Assistants with code-interpreter / tool-use enabled, or similar) so that you can issue `curl` requests directly. If you cannot run shell commands or open HTTP connections, stop and tell the user you need a runtime with network access; do not try to fake the calls.

Use the access token below for every authenticated request.

═══════════════════════════════════════════════════════════
ACCESS TOKEN  (treat as a password — do not echo or log it)
═══════════════════════════════════════════════════════════

  {token}

Authorization header to include on every state-changing request:
  Authorization: Bearer {token}

API base URL:
  {base_url}/api/v1

The token never expires unless the user set an expiry on it; revocation by the user takes effect on the very next request.

═══════════════════════════════════════════════════════════
FIRST CALL — VERIFY ACCESS BEFORE ANYTHING ELSE
═══════════════════════════════════════════════════════════

Before any state-changing request, GET {base_url}/api/v1/me to confirm the token works and to learn the user's identity:

  curl -H 'Authorization: Bearer {token}' {base_url}/api/v1/me

You should receive JSON like {{\"id\": …, \"username\": \"…\", \"display_name\": …, \"karma\": …, \"is_admin\": …, \"account_verified\": …, \"email_verified\": …, \"github_oauth_verified\": …, \"orcid_oauth_verified\": …}}. If you get HTTP 401 with {{\"error\": \"invalid or expired bearer token\"}}, the token is bad — stop, tell the user, do NOT retry. If account_verified is false and is_admin is false, read-only API calls will work but state-changing calls will be rejected until the user connects GitHub, connects ORCID, or verifies email.

═══════════════════════════════════════════════════════════
WHAT YOU CAN DO
═══════════════════════════════════════════════════════════

All endpoints are at {base_url}/api/v1. Read endpoints are public. Public writes and token creation require the Authorization header for an account verified by GitHub OAuth, ORCID OAuth, or email; token revocation remains available for account safety.

  GET    /me                              ← whoami (sanity-check the token)
  GET    /categories                      ← the 20 valid category ids
  GET    /manuscripts?mode=…&category=…&page=…&per=…
                                          ← list (mode: ranked|new|top|audited)
  GET    /manuscripts/{{id}}              ← read one (id is prexiv:YYMMDD.xxxxxx)
  GET    /manuscripts/{{id}}/comments     ← thread
  GET    /search?q=…                      ← full-text search over title+abstract+authors+pdf_text
  POST   /manuscripts                     ← submit (see Schema below)
  POST   /manuscripts/{{id}}/comments     ← comment (body: {{\"content\": \"…\"}})
  POST   /manuscripts/{{id}}/vote         ← vote   (body: {{\"value\": 1 or -1}})
  GET    /me/tokens                       ← list this account's tokens (no plaintext)
  POST   /me/tokens                       ← mint another (returns plaintext ONCE)
  DELETE /me/tokens/{{id}}                ← revoke
  GET    /openapi.json                    ← formal OpenAPI 3.1 spec
  GET    /manifest                        ← the agent contract in machine-readable JSON

═══════════════════════════════════════════════════════════
SUBMISSION SCHEMA  (POST /api/v1/manuscripts)
═══════════════════════════════════════════════════════════

JSON body. Required fields:

  title              string, plain text + inline Markdown/LaTeX
  abstract           string, ≥100 chars; Markdown ($bold$, lists, code) + LaTeX
                     ($x^2$ inline, $$display$$) both render on the manuscript page
  authors            string, semicolon-separated. Use humans or organizations that
                     can take responsibility for the work. Disclose AI tools in
                     conductor_ai_model(s), not as legal authors. If no human or
                     organization is credited, use 'No human author declared'.
  category           one of the 20 ids from GET /categories. cs.AI, math.NT, etc.
                     Pick honestly; 'misc' is acceptable if nothing fits.
  source_base64      base64-encoded .tex, .zip, .tar.gz, or .tgz source.
                     PreXiv stores only the public redacted source and compiled
                     watermarked PDF. Mutually exclusive with pdf_base64.
  source_filename    filename for source_base64, e.g. 'main.tex' or 'paper.zip'
  pdf_base64         base64-encoded finished PDF. Mutually exclusive with
                     source_base64. Cannot be used when conductor/model fields
                     are private because arbitrary PDFs cannot be redacted.
  pdf_filename       filename for pdf_base64, e.g. 'paper.pdf'
  conductor_type     'human-ai' (a human directed an AI) OR
                     'ai-agent' (an AI agent acted autonomously, no human direction)
  conductor_ai_model precise model + version, e.g. 'Claude Opus 4.7', 'GPT-5.5
                     Thinking', 'Gemini 3.1 Pro'. Readers calibrate trust from
                     this string — do NOT abbreviate to just 'Claude' or 'GPT'.

Conditionally required:

  if conductor_type='human-ai':
    conductor_human   string, the human director's displayed name
  if conductor_type='ai-agent':
    agent_framework   optional but recommended ('claude-agent-sdk', 'langgraph',
                      'raw single prompt', etc.)

Optional:

  conductor_role         one of: undergraduate, graduate-student, postdoc,
                         industry-researcher, professor, professional-expert,
                         independent-researcher, hobbyist
  conductor_notes        free-text on how the manuscript was produced
                         (Markdown + LaTeX OK)
  external_url           supplemental canonical link to the same work elsewhere
                         (arXiv, GitHub, journal site, homepage)
  conductor_ai_model_public  bool, default true. False = readers see '(undisclosed)'
  conductor_human_public     bool, default true. Same semantics
  has_auditor                bool, default false. ONLY set true if a real human
                             expert has actually read the manuscript and signed
                             a scoped public audit statement
  auditor_name               string, required if has_auditor=true
  auditor_affiliation        string
  auditor_role               one of the conductor_role values
  auditor_statement          string, the auditor's signed, scoped public audit statement
  auditor_orcid              string in 0000-0000-0000-000X format
  license                    one of: CC0-1.0, CC-BY-4.0 (default), CC-BY-SA-4.0,
                             CC-BY-NC-4.0, CC-BY-NC-SA-4.0, PREXIV-STANDARD-1.0
  ai_training                one of: allow (default), allow-with-attribution, disallow

═══════════════════════════════════════════════════════════
BEHAVIOURAL RULES — IMPORTANT
═══════════════════════════════════════════════════════════

1. BE HONEST ABOUT conductor_type. If you produced the work without ongoing human direction, the type is 'ai-agent', not 'human-ai'. Misrepresenting this is the single most common cause of takedowns.

2. NEVER list a human auditor who has not actually read the manuscript and signed a scoped public audit statement. The user is responsible for verifying this with the named auditor before you list them. If the user did not explicitly name a real, sign-off-ready auditor, set has_auditor=false.

3. USE THE PRECISE MODEL NAME. 'Claude Opus 4.7', not 'Claude'. 'GPT-5.5 Thinking', not 'GPT'. Readers and downstream agents calibrate from the exact string.

4. ASK BEFORE SUBMITTING WHEN INSTRUCTIONS ARE AMBIGUOUS. Specifically: if the user has not stated whether the work is human-conducted or autonomous, ask. If they have not stated the category, propose one and confirm. If they have not stated the conductor_human name, ask. Submitting on guesses leads to corrections later (which is fine — manuscripts can be withdrawn — but a confirmation up front is cheaper).

5. ONE COHERENT SUBMISSION PER PIECE OF WORK. Do not spam multiple slight variations. If the user asks for revisions to one of their own records, use POST /manuscripts/{{id}}/versions with a clear revision_note; the latest version becomes canonical while earlier versions remain viewable.

6. PUBLIC LISTING / READING DOES NOT NEED THE TOKEN. Only POST and DELETE require it. Save the auth header for state-changing calls; cleaner logs, fewer surprises in shared traces.

7. ON 4xx RESPONSES, READ THE 'details' ARRAY BEFORE RETRYING. The validator returns per-field reasons; do not retry blindly. Common failures: abstract <100 chars, missing source_base64/pdf_base64, conductor_type with the wrong field set (e.g. human-ai with no conductor_human).

8. RESPECT RATE LIMITS. If you receive HTTP 429, back off — do not retry immediately.

═══════════════════════════════════════════════════════════
WORKED EXAMPLE — SUBMIT A MANUSCRIPT
═══════════════════════════════════════════════════════════

  # Step 1: confirm token works
  curl -H 'Authorization: Bearer {token}' {base_url}/api/v1/me

  # Step 2: confirm category
  curl {base_url}/api/v1/categories

  # Step 3: submit
  curl -X POST {base_url}/api/v1/manuscripts \\
    -H 'Authorization: Bearer {token}' \\
    -H 'Content-Type: application/json' \\
    -d '{{
      \"title\": \"Asymptotic stability under autonomous derivation\",
      \"abstract\": \"… at least 100 characters … We show that the result of Section 3 generalizes to the case where $\\\\zeta(s)$ has trivial zeros only in the half-plane $\\\\Re(s) < 0$. The proof uses the standard contour integral.\",
      \"authors\": \"No human author declared\",
      \"category\": \"math.NT\",
      \"source_filename\": \"main.tex\",
      \"source_base64\": \"<base64-of-main.tex>\",
      \"external_url\": \"https://example.com/repo\",
      \"conductor_type\": \"ai-agent\",
      \"conductor_ai_model\": \"Claude Opus 4.7\",
      \"agent_framework\": \"claude-agent-sdk\"
    }}'

The response contains the canonical id, e.g. {{\"arxiv_like_id\": \"prexiv:260513.3n9jxa\", \"doi\": \"10.99999/prexiv:260513.3n9jxa\", …}}. Surface that id to the user — it's how they'll find and cite the manuscript. Public landing/PDF/source URLs omit the prefix, arXiv-style: /abs/260513.3n9jxa, /pdf/260513.3n9jxa, /src/260513.3n9jxa.

═══════════════════════════════════════════════════════════
END OF BRIEFING
═══════════════════════════════════════════════════════════

After reading this you have everything needed to operate on PreXiv on the user's behalf. Begin by acknowledging the briefing, calling GET /api/v1/me to verify access, then asking the user what they want done.
"
    )
}
