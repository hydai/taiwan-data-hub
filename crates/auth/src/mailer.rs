//! Pluggable mailer.
//!
//! `Mailer` is a small trait covering the two flows #4.2 needs:
//! verification + password-reset magic links. Sender details are
//! held by the implementor — the service does not see SMTP creds.
//!
//! Three implementations live here:
//!
//! - `SmtpMailer` — production path, lettre's async SMTP transport.
//! - `LogMailer` — personal-mode fallback that logs the link to
//!   `tracing::warn!` instead of sending mail, so a laptop install
//!   without SMTP can still complete the verify-by-clicking flow
//!   by reading the log.
//! - `MemoryMailer` — test double that records every send for
//!   assertions. Exposed unconditionally (NOT behind `cfg(test)`)
//!   so dependent crates can use it in their own integration
//!   tests; #4.5's gateway HTTP tests are the first consumer.
//!
//! The crate intentionally does NOT shell out to provider-specific
//! HTTP APIs (Resend, Postmark, …). Those all expose an SMTP
//! endpoint, so a single `SMTP_URL` env knob covers every provider.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use lettre::message::Mailbox;
use lettre::message::header::ContentType;
use lettre::transport::smtp::AsyncSmtpTransport;
use lettre::{AsyncTransport, Message, Tokio1Executor};
use tracing::warn;
use url::Url;

use crate::error::AuthError;

/// The two transactional emails #4.2 needs.
///
/// Both methods take `expires_in` so the body's "expires in N
/// hours" copy stays accurate when the caller overrides the
/// service-level TTL via `AuthService::with_verify_ttl` or
/// `with_reset_ttl`.
#[async_trait]
pub trait Mailer: Send + Sync {
    /// Send the email-verification magic link to a newly-registered
    /// user. `expires_in` is rendered into the email body.
    async fn send_verification(
        &self,
        to: &str,
        link: &Url,
        expires_in: Duration,
    ) -> Result<(), AuthError>;
    /// Send the password-reset magic link. `expires_in` is rendered
    /// into the email body.
    async fn send_password_reset(
        &self,
        to: &str,
        link: &Url,
        expires_in: Duration,
    ) -> Result<(), AuthError>;
}

/// Address used as the `From:` on outgoing mail.
///
/// Held separately from the transport so a single transport (e.g.
/// a shared Postmark account) can serve multiple `Mailer`
/// instances with different sender identities. The `Reply-To:`
/// header is intentionally NOT set — transactional mail bounces
/// back to the `From:` mailbox, and a separate reply-to channel
/// is not part of the #4.2 surface.
#[derive(Debug, Clone)]
pub struct MailFrom {
    pub address: String,
    pub display_name: Option<String>,
}

impl MailFrom {
    fn to_mailbox(&self) -> Result<Mailbox, AuthError> {
        let parsed: lettre::Address = self
            .address
            .parse()
            .map_err(|e: lettre::address::AddressError| AuthError::Mailer(e.to_string()))?;
        Ok(match &self.display_name {
            Some(name) => Mailbox::new(Some(name.clone()), parsed),
            None => Mailbox::new(None, parsed),
        })
    }
}

/// Production mailer backed by lettre's async SMTP transport.
pub struct SmtpMailer {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: MailFrom,
}

impl SmtpMailer {
    /// Build a transport from a connection URL like
    /// `smtp://user:pass@host:587` or `smtps://user:pass@host:465`.
    ///
    /// `smtp://` implicitly enables STARTTLS when the server
    /// advertises it; `smtps://` is implicit-TLS. Both shapes are
    /// what Resend / Postmark / Mailgun publish.
    pub fn new(connection_url: &str, from: MailFrom) -> Result<Self, AuthError> {
        // `from_url` configures TLS from the scheme + host already:
        //   * `smtp://host:587`  → opportunistic STARTTLS keyed off `host`
        //   * `smtps://host:465` → implicit TLS keyed off `host`
        //
        // We deliberately don't override `.tls(...)` here — a previous
        // attempt did, with a hardcoded `localhost` SNI, which made
        // certificate verification fail against every real provider.
        let transport = AsyncSmtpTransport::<Tokio1Executor>::from_url(connection_url)
            .map_err(|e| AuthError::Mailer(format!("invalid SMTP URL: {e}")))?
            .build();
        Ok(Self { transport, from })
    }

    async fn send(&self, to: &str, subject: &str, body: String) -> Result<(), AuthError> {
        let to_addr: lettre::Address = to
            .parse()
            .map_err(|e: lettre::address::AddressError| AuthError::Mailer(e.to_string()))?;
        // The verification + reset bodies include non-ASCII (em
        // dash etc.), so the Content-Type MUST carry an explicit
        // charset — `ContentType::TEXT_PLAIN` alone produces no
        // charset and some MTAs/clients misinterpret the bytes.
        let content_type = ContentType::parse("text/plain; charset=utf-8")
            .expect("text/plain; charset=utf-8 is a static valid MIME");
        let message = Message::builder()
            .from(self.from.to_mailbox()?)
            .to(Mailbox::new(None, to_addr))
            .subject(subject)
            .header(content_type)
            .body(body)
            .map_err(|e| AuthError::Mailer(e.to_string()))?;
        self.transport
            .send(message)
            .await
            .map_err(|e| AuthError::Mailer(e.to_string()))?;
        Ok(())
    }
}

#[async_trait]
impl Mailer for SmtpMailer {
    async fn send_verification(
        &self,
        to: &str,
        link: &Url,
        expires_in: Duration,
    ) -> Result<(), AuthError> {
        self.send(to, "Verify your email", verification_body(link, expires_in))
            .await
    }

    async fn send_password_reset(
        &self,
        to: &str,
        link: &Url,
        expires_in: Duration,
    ) -> Result<(), AuthError> {
        self.send(to, "Reset your password", reset_body(link, expires_in))
            .await
    }
}

/// Personal-mode fallback: print magic links to logs instead of
/// sending email. Lets a laptop install without SMTP credentials
/// still complete the verify-by-clicking flow.
///
/// ⚠️ The full link (including the secret `?token=…`) is logged
/// ONLY when `reveal_token` is `true`. That mode is for local
/// personal-mode dev: a production deployment that ships logs to
/// a central aggregator would otherwise effectively persist
/// single-use credentials. `Default` is `reveal_token = false`
/// — the redacted form omits the query string so the link is
/// useless on its own. Use [`LogMailer::personal_mode_reveal`]
/// to opt into the full-link form explicitly.
#[derive(Debug, Default, Clone, Copy)]
pub struct LogMailer {
    reveal_token: bool,
}

impl LogMailer {
    /// Redacted form: logs the link with the `?token=…` query
    /// stripped. Safe to enable in any deployment but the
    /// resulting log line is not enough to complete the flow.
    #[must_use]
    pub const fn redacting() -> Self {
        Self {
            reveal_token: false,
        }
    }

    /// Reveal form: logs the FULL magic link including the secret
    /// token. ONLY for personal-mode dev — production deployments
    /// must use [`SmtpMailer`].
    #[must_use]
    pub const fn personal_mode_reveal() -> Self {
        Self { reveal_token: true }
    }

    fn rendered_link(self, link: &Url) -> String {
        if self.reveal_token {
            link.to_string()
        } else {
            // Strip the query string so the token doesn't land in
            // log aggregators. Operators still see which path the
            // link targeted, which is enough for "did the email
            // get triggered?" debugging.
            let mut redacted = link.clone();
            redacted.set_query(None);
            format!("{redacted}?token=<redacted>")
        }
    }
}

#[async_trait]
impl Mailer for LogMailer {
    async fn send_verification(
        &self,
        to: &str,
        link: &Url,
        expires_in: Duration,
    ) -> Result<(), AuthError> {
        warn!(
            recipient = to,
            link = %self.rendered_link(link),
            expires_in = ?expires_in,
            reveal_token = self.reveal_token,
            "SMTP not configured; printing verification link to logs"
        );
        Ok(())
    }

    async fn send_password_reset(
        &self,
        to: &str,
        link: &Url,
        expires_in: Duration,
    ) -> Result<(), AuthError> {
        warn!(
            recipient = to,
            link = %self.rendered_link(link),
            expires_in = ?expires_in,
            reveal_token = self.reveal_token,
            "SMTP not configured; printing password-reset link to logs"
        );
        Ok(())
    }
}

/// One recorded send. Held by [`MemoryMailer`] so tests can assert
/// "was a verification email sent to X" and inspect the link +
/// effective TTL.
#[derive(Debug, Clone)]
pub struct SentMessage {
    pub to: String,
    pub kind: MailKind,
    pub link: Url,
    pub expires_in: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailKind {
    Verification,
    PasswordReset,
}

/// Test double that records every send into an internal `Vec`.
///
/// Public so integration tests in dependent crates (and the
/// gateway HTTP tests in #4.5) can use it.
#[derive(Default, Clone)]
pub struct MemoryMailer {
    sent: Arc<Mutex<Vec<SentMessage>>>,
}

impl MemoryMailer {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot of every send so far. The returned `Vec` is a clone
    /// — modifying it does not affect the mailer's record.
    pub fn sent(&self) -> Vec<SentMessage> {
        self.sent
            .lock()
            .expect("MemoryMailer mutex poisoned")
            .clone()
    }

    fn record(&self, msg: SentMessage) {
        self.sent
            .lock()
            .expect("MemoryMailer mutex poisoned")
            .push(msg);
    }
}

#[async_trait]
impl Mailer for MemoryMailer {
    async fn send_verification(
        &self,
        to: &str,
        link: &Url,
        expires_in: Duration,
    ) -> Result<(), AuthError> {
        self.record(SentMessage {
            to: to.to_owned(),
            kind: MailKind::Verification,
            link: link.clone(),
            expires_in,
        });
        Ok(())
    }

    async fn send_password_reset(
        &self,
        to: &str,
        link: &Url,
        expires_in: Duration,
    ) -> Result<(), AuthError> {
        self.record(SentMessage {
            to: to.to_owned(),
            kind: MailKind::PasswordReset,
            link: link.clone(),
            expires_in,
        });
        Ok(())
    }
}

fn verification_body(link: &Url, expires_in: Duration) -> String {
    let when = humanise_duration(expires_in);
    format!(
        "Welcome to Taiwan Data Hub.\n\n\
         To finish creating your account, open the link below.\n\
         The link is single-use and will expire in {when}.\n\n\
         {link}\n\n\
         If you didn't sign up, you can safely ignore this email."
    )
}

fn reset_body(link: &Url, expires_in: Duration) -> String {
    let when = humanise_duration(expires_in);
    format!(
        "Someone (hopefully you) requested a password reset for your\n\
         Taiwan Data Hub account. Open the link below to choose a new\n\
         password. The link is single-use and expires in {when}.\n\n\
         {link}\n\n\
         If you didn't request a reset, you can ignore this email —\n\
         your existing password will keep working."
    )
}

/// Render a `Duration` as the lowest-precision plain-English string
/// that loses no information *down to a one-second granularity*:
/// "24 hours", "30 minutes", "45 seconds". Sub-second components
/// are truncated (`as_secs`) because the surrounding TTLs are
/// always whole seconds. Used by the email-body templates so the
/// "expires in N" copy always matches the effective TTL.
fn humanise_duration(d: Duration) -> String {
    let secs = d.as_secs();
    if secs >= 3600 && secs % 3600 == 0 {
        let h = secs / 3600;
        return format!("{h} hour{}", if h == 1 { "" } else { "s" });
    }
    if secs >= 60 && secs % 60 == 0 {
        let m = secs / 60;
        return format!("{m} minute{}", if m == 1 { "" } else { "s" });
    }
    format!("{secs} second{}", if secs == 1 { "" } else { "s" })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_TTL: Duration = Duration::from_secs(24 * 60 * 60);

    #[tokio::test]
    async fn memory_mailer_records_verification_send() {
        let mailer = MemoryMailer::new();
        let link = Url::parse("https://hub.example/verify?token=abc").unwrap();
        mailer
            .send_verification("u@example.com", &link, TEST_TTL)
            .await
            .unwrap();
        let sent = mailer.sent();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].to, "u@example.com");
        assert_eq!(sent[0].kind, MailKind::Verification);
        assert_eq!(sent[0].link, link);
        assert_eq!(sent[0].expires_in, TEST_TTL);
    }

    #[tokio::test]
    async fn memory_mailer_records_password_reset_send() {
        let mailer = MemoryMailer::new();
        let link = Url::parse("https://hub.example/reset?token=def").unwrap();
        mailer
            .send_password_reset("u@example.com", &link, Duration::from_secs(3600))
            .await
            .unwrap();
        let sent = mailer.sent();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].kind, MailKind::PasswordReset);
        assert_eq!(sent[0].expires_in, Duration::from_secs(3600));
    }

    #[tokio::test]
    async fn log_mailer_returns_ok_without_smtp() {
        let mailer = LogMailer::redacting();
        let link = Url::parse("https://hub.example/verify?token=abc").unwrap();
        mailer
            .send_verification("u@example.com", &link, TEST_TTL)
            .await
            .unwrap();
        mailer
            .send_password_reset("u@example.com", &link, TEST_TTL)
            .await
            .unwrap();
    }

    #[test]
    fn humanise_duration_picks_lowest_precision() {
        assert_eq!(humanise_duration(Duration::from_secs(3600)), "1 hour");
        assert_eq!(humanise_duration(Duration::from_secs(86_400)), "24 hours");
        assert_eq!(humanise_duration(Duration::from_secs(60)), "1 minute");
        assert_eq!(humanise_duration(Duration::from_secs(900)), "15 minutes");
        assert_eq!(humanise_duration(Duration::from_secs(45)), "45 seconds");
        assert_eq!(humanise_duration(Duration::from_secs(1)), "1 second");
    }

    #[test]
    fn verification_body_uses_provided_ttl() {
        let link = Url::parse("https://hub.example/verify?token=t").unwrap();
        assert!(verification_body(&link, Duration::from_secs(3600)).contains("expire in 1 hour"));
        assert!(
            verification_body(&link, Duration::from_secs(86_400)).contains("expire in 24 hours")
        );
    }

    #[test]
    fn mail_from_renders_with_display_name() {
        let from = MailFrom {
            address: "bot@hub.example".to_owned(),
            display_name: Some("Taiwan Data Hub".to_owned()),
        };
        let mb = from.to_mailbox().unwrap();
        // lettre's Mailbox Display omits angle brackets when name is None;
        // with a name it renders as `Name <addr>`.
        assert!(format!("{mb}").contains("Taiwan Data Hub"));
        assert!(format!("{mb}").contains("bot@hub.example"));
    }
}
