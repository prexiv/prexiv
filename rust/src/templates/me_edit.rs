use maud::{html, Markup};

use super::layout::{layout, PageCtx};
use crate::routes::me_edit::EditValues;

// `orcid_flash` carries inline feedback from a recent ORCID OAuth
// attempt — `(message, is_error)`, or None when nothing's fresh.
// Rendered inside the verification status panel so users don't
// have to scroll back up to discover what happened.
pub struct VerificationUi<'a> {
    pub github_flash: Option<(&'a str, bool)>,
    pub github_oauth_unavailable: Option<&'a str>,
    pub orcid_flash: Option<(&'a str, bool)>,
    pub orcid_oauth_unavailable: Option<&'a str>,
}

pub fn render(
    ctx: &PageCtx,
    v: &EditValues,
    errors: &[String],
    pending_new_email: Option<&str>,
    verification: VerificationUi<'_>,
) -> Markup {
    let VerificationUi {
        github_flash,
        github_oauth_unavailable,
        orcid_flash,
        orcid_oauth_unavailable,
    } = verification;
    let user = ctx.user.as_ref();
    let username = user.map(|u| u.username.as_str()).unwrap_or("");
    let email = user.map(|u| u.email.as_str()).unwrap_or("");
    let email_verified = user.map(|u| u.is_verified()).unwrap_or(false);
    let account_verified = user.map(|u| u.is_verified_or_admin()).unwrap_or(false);
    let display_name_current = user.and_then(|u| u.display_name.as_deref()).unwrap_or("");
    let affiliation_current = user.and_then(|u| u.affiliation.as_deref()).unwrap_or("");
    let created_at = user.and_then(|u| u.created_at);
    let created_at_str = created_at
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_default();

    let body = html! {
        div.page-header {
            h1 { "Edit profile" }
            p.muted {
                "Tweak your account settings. Display fields apply to past and future submissions where you appear as the submitter."
            }
        }

        // arxiv-style summary panel: read-only key fields + action links.
        section.account-info-panel aria-label="Account information" {
            h2.account-info-title { "Account information" }
            dl.account-info-grid {
                dt { "E-mail" }
                dd {
                    span.account-info-email.no-katex { (email) }
                    @if email_verified {
                        span.account-badge.account-badge-ok { "✓ verified" }
                    } @else {
                        span.account-badge.account-badge-warn { "⚠ not verified" }
                    }
                }
                dt { "Account status" }
                dd {
                    @if account_verified {
                        span.account-badge.account-badge-ok { "✓ write-enabled" }
                    } @else {
                        span.account-badge.account-badge-warn { "⚠ verification needed" }
                    }
                }
                dt { "Username" }
                dd { code.no-katex { (username) } }
                @if !display_name_current.is_empty() {
                    dt { "Display name" }
                    dd { (display_name_current) }
                }
                @if !affiliation_current.is_empty() {
                    dt { "Affiliation" }
                    dd { (affiliation_current) }
                }
                @if !created_at_str.is_empty() {
                    dt { "Member since" }
                    dd { (created_at_str) }
                }
            }
            nav.account-info-actions aria-label="Account actions" {
                a.account-info-link href="/me/email"    { "Change email" }
                span.account-info-sep { "|" }
                a.account-info-link href="/me/password" { "Change password" }
                span.account-info-sep { "|" }
                a.account-info-link href="/me/2fa"      { "Two-factor auth" }
                span.account-info-sep { "|" }
                a.account-info-link href="/me/tokens"   { "API tokens" }
                span.account-info-sep { "|" }
                a.account-info-link href="/me/export"   { "Export data" }
                span.account-info-sep { "|" }
                a.account-info-link.account-info-danger href="/me/delete-account" { "Delete account" }
            }
        }

        @if !account_verified {
            (verify_banner(&ctx.csrf_token, email, ctx.pending_verify_token.as_deref()))
        }

        @if let Some(pending) = pending_new_email {
            (email_change_banner(&ctx.csrf_token, pending, ctx.pending_email_change_token.as_deref()))
        }

        @if !errors.is_empty() {
            div.form-errors {
                strong { "Please fix the following:" }
                ul { @for e in errors { li { (e) } } }
            }
        }

        form.submit-form method="post" action="/me/edit" {
            input type="hidden" name="csrf_token" value=(ctx.csrf_token);

            section.form-section {
                h2 { "Display" }
                label {
                    span.label-text { "Display name" }
                    input type="text" name="display_name" maxlength="200" value=(v.display_name)
                          placeholder="Jane Doe";
                    span.hint { "Optional. Shown alongside your username on manuscripts and comments." }
                }
                label {
                    span.label-text { "Affiliation" }
                    input type="text" name="affiliation" maxlength="200" value=(v.affiliation)
                          placeholder="MIT — Lab for Plausible Theorems";
                    span.hint { "Free-form. Where (or if) you do this work." }
                }
                label {
                    span.label-text { "Bio" }
                    textarea name="bio" maxlength="2000" rows="4"
                             placeholder="Optional. A few sentences about yourself." { (v.bio) }
                }
            }

            section.form-section {
                h2 {
                    "Verification and listing status"
                }
                p.muted.small.no-katex {
                    "PreXiv's default ranked listings (" code { "/" } " " code { "/new" } " "
                    code { "/top" } " " code { "/audited" }
                    ") only surface manuscripts from "
                    strong { "verified accounts" }
                    ". Verification means GitHub OAuth, ORCID OAuth, or email verification. ORCID and institutional email remain stronger identity signals, but GitHub now has the same posting and default-listing rights. Other work remains reachable via "
                    code { "/browse" }
                    " and search."
                }
                (verified_scholar_status_panel(user, orcid_flash))
            }

            section.form-section {
                h2 { "GitHub account verification" }
                p.muted.small {
                    "Connect through GitHub OAuth to unlock submissions, default listings, comments, votes, and API-token minting. PreXiv stores your GitHub numeric id and login, then discards the temporary OAuth token."
                }
                div.verified-scholar-panel {
                    @if let Some((msg, is_err)) = github_flash {
                        @let cls = if is_err { "vsp-flash vsp-flash-err" } else { "vsp-flash vsp-flash-ok" };
                        div.(cls) role="status" aria-live="polite" {
                            @if is_err {
                                span.vsp-flash-icon aria-hidden="true" { "⚠" }
                            } @else {
                                span.vsp-flash-icon aria-hidden="true" { "✓" }
                            }
                            span.vsp-flash-msg { (msg) }
                        }
                    }
                    (github_status_row(user))
                }
                div.orcid-oauth-card {
                    div {
                        strong { "Account-control proof" }
                        p.muted.small.no-katex {
                            @if let Some(msg) = github_oauth_unavailable {
                                (msg)
                            } @else {
                                "You will be sent to github.com, sign in there, and authorize PreXiv to read your public GitHub identity."
                            }
                        }
                    }
                    @if github_oauth_unavailable.is_some() {
                        button.btn-secondary type="button" disabled { "GitHub not configured" }
                    } @else {
                        a.btn-primary href="/me/github/connect" { "Connect with GitHub" }
                    }
                }
            }

            section.form-section {
                h2 { "ORCID " span.muted { "(optional)" } }
                p.muted.small {
                    "Connect through ORCID OAuth to prove that this PreXiv account controls the ORCID iD. PreXiv does not accept pasted ORCID iDs as verification."
                }
                div.orcid-oauth-card {
                    div {
                        strong { "Authenticated ORCID binding" }
                        p.muted.small.no-katex {
                            @if let Some(msg) = orcid_oauth_unavailable {
                                (msg)
                            } @else {
                                "You will be sent to orcid.org, sign in there, and authorize PreXiv to receive your authenticated ORCID iD."
                            }
                        }
                    }
                    @if orcid_oauth_unavailable.is_some() {
                        button.btn-secondary type="button" disabled { "ORCID not configured" }
                    } @else {
                        a.btn-primary href="/me/orcid/connect" { "Connect with ORCID" }
                    }
                }
            }

            div.form-submit {
                button.btn-primary.big type="submit" { "Save changes" }
                " "
                a.btn-secondary href={ "/u/" (username) } { "Cancel" }
            }
        }
    };
    layout("Edit profile", ctx, body)
}

/// Status panel — read-only summary of the ownership-grade
/// identity-verification signals. The ORCID action lives down in the ORCID
/// section and always goes through OAuth.
fn verified_scholar_status_panel(
    user: Option<&crate::models::User>,
    orcid_flash: Option<(&str, bool)>,
) -> Markup {
    let orcid_oauth = user.map(|u| u.is_orcid_oauth_verified()).unwrap_or(false);
    let inst_email = user
        .map(|u| u.is_verified() && u.is_institutional_email())
        .unwrap_or(false);
    let stored_orcid = user.and_then(|u| u.orcid.as_deref()).unwrap_or("");
    html! {
        div.verified-scholar-panel {
            @if let Some((msg, is_err)) = orcid_flash {
                @let cls = if is_err { "vsp-flash vsp-flash-err" } else { "vsp-flash vsp-flash-ok" };
                div.(cls) role="status" aria-live="polite" {
                    @if is_err {
                        span.vsp-flash-icon aria-hidden="true" { "⚠" }
                    } @else {
                        span.vsp-flash-icon aria-hidden="true" { "✓" }
                    }
                    span.vsp-flash-msg { (msg) }
                }
            }
            div.vsp-row {
                div.vsp-row-label {
                    strong { "Institutional email" }
                    span.muted.small.no-katex {
                        "Requires a verified email address on an institutional / R&D-org domain."
                    }
                }
                div.vsp-row-status {
                    @if inst_email {
                        span.vsp-pill.vsp-pill-ok { "✓ verified" }
                    } @else {
                        span.vsp-pill.vsp-pill-pending { "not yet" }
                    }
                }
            }
            div.vsp-row {
                div.vsp-row-label {
                    strong { "Authenticated ORCID" }
                    @if orcid_oauth && !stored_orcid.is_empty() {
                        " " code.no-katex { (stored_orcid) }
                    }
                    span.muted.small.no-katex {
                        @if orcid_oauth {
                            "Connected through ORCID OAuth. This unlocks public writes, default listings, and a stronger identity signal."
                        } @else {
                            "Not connected. Use "
                            strong { "Connect with ORCID" }
                            " below for ownership-grade verification."
                        }
                    }
                }
                div.vsp-row-status {
                    @if orcid_oauth {
                        span.vsp-pill.vsp-pill-ok { "authenticated" }
                    } @else {
                        span.vsp-pill.vsp-pill-pending { "not connected" }
                    }
                }
            }
        }
    }
}

fn github_status_row(user: Option<&crate::models::User>) -> Markup {
    let github_verified = user.map(|u| u.is_github_oauth_verified()).unwrap_or(false);
    let login = user
        .and_then(|u| u.github_login.as_deref())
        .unwrap_or("")
        .trim();
    html! {
        div.vsp-row {
            div.vsp-row-label {
                strong { "Authenticated GitHub" }
                @if github_verified && !login.is_empty() {
                    " " code.no-katex { "@" (login) }
                }
                span.muted.small.no-katex {
                    @if github_verified {
                        "Connected through GitHub OAuth. This unlocks public writes, default listings, and API-token minting."
                    } @else {
                        "Not connected. This is the recommended account-verification path when email delivery is unavailable."
                    }
                }
            }
            div.vsp-row-status {
                @if github_verified {
                    span.vsp-pill.vsp-pill-ok { "write-enabled" }
                } @else {
                    span.vsp-pill.vsp-pill-pending { "not connected" }
                }
            }
        }
    }
}

/// Banner shown at the top of /me/edit (and /submit) when the current
/// user's account is not write-enabled yet.
pub fn verify_banner(csrf_token: &str, email: &str, pending_token: Option<&str>) -> Markup {
    html! {
        div.verify-banner role="status" {
            @if let Some(token) = pending_token {
                div.verify-banner-text {
                    strong { "Account verification required." }
                    " "
                    "Connect GitHub on "
                    a href="/me/edit" { "your profile" }
                    " to unlock public writes, or use the email fallback below. A verification email was sent to "
                    strong { (email) }
                    "."
                }
                div.verify-banner-actions {
                    a.btn-primary href="/me/edit" { "Connect GitHub" }
                    a.btn-secondary href={ "/verify/" (token) } { "Verify email" }
                    form.verify-banner-resend method="post" action="/me/resend-verification" {
                        input type="hidden" name="csrf_token" value=(csrf_token);
                        button.btn-secondary type="submit" { "New link" }
                    }
                }
            } @else {
                div.verify-banner-text {
                    strong { "Account verification required." }
                    " "
                    "Connect GitHub on "
                    a href="/me/edit" { "your profile" }
                    " to unlock public writes. Email verification is still available as a fallback; we sent a link to "
                    strong { (email) }
                    " when you registered."
                }
                form.verify-banner-resend method="post" action="/me/resend-verification" {
                    input type="hidden" name="csrf_token" value=(csrf_token);
                    button.btn-secondary type="submit" { "Resend verification" }
                }
            }
        }
    }
}

/// Banner shown when an email-change is pending. Carries the inline
/// confirm button if the session has the plaintext token (just-minted
/// after submitting the form), plus a Cancel control.
pub fn email_change_banner(
    csrf_token: &str,
    new_email: &str,
    pending_token: Option<&str>,
) -> Markup {
    html! {
        div.verify-banner.verify-banner-info role="status" {
            @if let Some(token) = pending_token {
                div.verify-banner-text {
                    strong { "Pending email change to " (new_email) }
                    " — click below to confirm. A confirmation email was also sent to that address."
                }
                div.verify-banner-actions {
                    a.btn-primary href={ "/confirm-email-change/" (token) } { "Confirm new email →" }
                    form.verify-banner-resend method="post" action="/me/email/cancel" {
                        input type="hidden" name="csrf_token" value=(csrf_token);
                        button.btn-secondary type="submit" { "Cancel" }
                    }
                }
            } @else {
                div.verify-banner-text {
                    strong { "Pending email change to " (new_email) }
                    ". A confirmation link is in your inbox — click it to finish. To get a fresh link instead, "
                    a href="/me/email" { "re-request the change" }
                    "."
                }
                form.verify-banner-resend method="post" action="/me/email/cancel" {
                    input type="hidden" name="csrf_token" value=(csrf_token);
                    button.btn-secondary type="submit" { "Cancel change" }
                }
            }
        }
    }
}
