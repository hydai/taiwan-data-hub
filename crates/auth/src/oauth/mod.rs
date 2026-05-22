//! OAuth 2.1 with PKCE + CSRF state.
//!
//! `OAuthService` is generic over a provider impl —
//! `OAuthService<GitHubProvider>` for #4.3,
//! `OAuthService<GoogleProvider>` for #4.4. The service itself
//! drives the two-leg authorization-code flow:
//!
//! 1. `start_login(redirect_uri)` mints a PKCE code-verifier +
//!    a CSRF `state` token, persists them in `oauth_states`,
//!    and returns the provider's authorize URL.
//! 2. `finish_login(code, state, redirect_uri)` consumes the
//!    matching `oauth_states` row, exchanges the code for an
//!    access token (sending `code_verifier`), fetches the
//!    provider's user profile, and links it to an existing user
//!    by verified email — creating one if no match exists.
//!
//! The access token (and refresh token, if any) is AES-256-GCM
//! encrypted before it lands in `oauth_accounts` (see [`crypto`]).
//!
//! #4.3 ships the [`github::GitHubProvider`] impl; #4.4 adds
//! Google via the same `OAuthProvider` trait.

pub mod crypto;
pub mod github;
pub mod pkce;
pub mod provider;
pub mod service;
pub mod state;

pub use crypto::{TokenCipher, TokenCipherError};
pub use github::GitHubProvider;
pub use pkce::{PkcePair, generate_pkce};
pub use provider::{OAuthProvider, ProviderProfile};
pub use service::{OAuthService, StartLogin};
pub use state::{StateToken, generate_state, hash_state};
