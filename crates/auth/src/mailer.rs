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
//!   assertions; only compiled in `#[cfg(test)]`.
//!
//! The crate intentionally does NOT shell out to provider-specific
//! HTTP APIs (Resend, Postmark, …). Those all expose an SMTP
//! endpoint, so a single `SMTP_URL` env knob covers every provider.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use lettre::message::Mailbox;
use lettre::message::header::ContentType;
use lettre::transport::smtp::AsyncSmtpTransport;
use lettre::{AsyncTransport, Message, Tokio1Executor};
use tracing::warn;
use url::Url;

use crate::error::AuthError;

/// The two transactional emails #4.2 needs.
#[async_trait]
pub trait Mailer: Send + Sync {
    /// Send the email-verification magic link to a newly-registered
    /// user.
    async fn send_verification(&self, to: &str, link: &Url) -> Result<(), AuthError>;
    /// Send the password-reset magic link.
    async fn send_password_reset(&self, to: &str, link: &Url) -> Result<(), AuthError>;
}

/// Address used as the `From:` and `Reply-To:` on outgoing mail.
/// Held separately from the transport so a single transport (e.g.
/// a shared Postmark account) can serve multiple `Mailer`
/// instances with different sender identities.
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
        let message = Message::builder()
            .from(self.from.to_mailbox()?)
            .to(Mailbox::new(None, to_addr))
            .subject(subject)
            .header(ContentType::TEXT_PLAIN)
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
    async fn send_verification(&self, to: &str, link: &Url) -> Result<(), AuthError> {
        self.send(to, "Verify your email", verification_body(link))
            .await
    }

    async fn send_password_reset(&self, to: &str, link: &Url) -> Result<(), AuthError> {
        self.send(to, "Reset your password", reset_body(link)).await
    }
}

/// Personal-mode fallback: print magic links to logs instead of
/// sending email. Lets a laptop install without SMTP credentials
/// still complete the verify-by-clicking flow.
pub struct LogMailer;

#[async_trait]
impl Mailer for LogMailer {
    async fn send_verification(&self, to: &str, link: &Url) -> Result<(), AuthError> {
        warn!(
            recipient = to,
            link = %link,
            "SMTP not configured; printing verification link to logs"
        );
        Ok(())
    }

    async fn send_password_reset(&self, to: &str, link: &Url) -> Result<(), AuthError> {
        warn!(
            recipient = to,
            link = %link,
            "SMTP not configured; printing password-reset link to logs"
        );
        Ok(())
    }
}

/// One recorded send. Held by [`MemoryMailer`] so tests can assert
/// "was a verification email sent to X" and inspect the link.
#[derive(Debug, Clone)]
pub struct SentMessage {
    pub to: String,
    pub kind: MailKind,
    pub link: Url,
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
    async fn send_verification(&self, to: &str, link: &Url) -> Result<(), AuthError> {
        self.record(SentMessage {
            to: to.to_owned(),
            kind: MailKind::Verification,
            link: link.clone(),
        });
        Ok(())
    }

    async fn send_password_reset(&self, to: &str, link: &Url) -> Result<(), AuthError> {
        self.record(SentMessage {
            to: to.to_owned(),
            kind: MailKind::PasswordReset,
            link: link.clone(),
        });
        Ok(())
    }
}

fn verification_body(link: &Url) -> String {
    format!(
        "Welcome to Taiwan Data Hub.\n\n\
         To finish creating your account, open the link below.\n\
         The link is single-use and will expire in 24 hours.\n\n\
         {link}\n\n\
         If you didn't sign up, you can safely ignore this email."
    )
}

fn reset_body(link: &Url) -> String {
    format!(
        "Someone (hopefully you) requested a password reset for your\n\
         Taiwan Data Hub account. Open the link below to choose a new\n\
         password. The link is single-use and expires in 1 hour.\n\n\
         {link}\n\n\
         If you didn't request a reset, you can ignore this email —\n\
         your existing password will keep working."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn memory_mailer_records_verification_send() {
        let mailer = MemoryMailer::new();
        let link = Url::parse("https://hub.example/verify?token=abc").unwrap();
        mailer
            .send_verification("u@example.com", &link)
            .await
            .unwrap();
        let sent = mailer.sent();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].to, "u@example.com");
        assert_eq!(sent[0].kind, MailKind::Verification);
        assert_eq!(sent[0].link, link);
    }

    #[tokio::test]
    async fn memory_mailer_records_password_reset_send() {
        let mailer = MemoryMailer::new();
        let link = Url::parse("https://hub.example/reset?token=def").unwrap();
        mailer
            .send_password_reset("u@example.com", &link)
            .await
            .unwrap();
        let sent = mailer.sent();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].kind, MailKind::PasswordReset);
    }

    #[tokio::test]
    async fn log_mailer_returns_ok_without_smtp() {
        let mailer = LogMailer;
        let link = Url::parse("https://hub.example/verify?token=abc").unwrap();
        mailer
            .send_verification("u@example.com", &link)
            .await
            .unwrap();
        mailer
            .send_password_reset("u@example.com", &link)
            .await
            .unwrap();
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
