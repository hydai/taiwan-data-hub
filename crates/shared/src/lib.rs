//! Cross-crate primitives shared by the gateway, workers, stdio shim,
//! and (eventually) the auth + i18n crates. Keep this crate tiny —
//! anything domain-specific belongs closer to its caller.
//!
//! Current surface:
//!
//! - [`Mode`] / [`ModeParseError`] / [`MODE_ENV`] — `MODE=personal|multi-user`
//!   parsing for every binary that needs to gate behavior on the
//!   operating mode (#4.1).

pub mod mode;

pub use mode::{MODE_ENV, Mode, ModeParseError};
