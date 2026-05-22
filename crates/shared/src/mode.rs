//! Process-wide operating mode (`MODE=personal|multi-user`).
//!
//! Lives in `shared` because every long-running binary (gateway,
//! mcp-stdio, etl-worker) and every middleware crate (`auth` once it
//! lands in M4) needs to read the same value and agree on which
//! strings count. Centralising the parser keeps the matrix small —
//! one place defines aliases, one place defines the default.

use std::env;
use std::fmt;
use std::str::FromStr;

use thiserror::Error;

/// Environment variable name read by [`Mode::from_env`].
pub const MODE_ENV: &str = "MODE";

/// Whether the deployment runs as a single-user laptop install or
/// a multi-user public service.
///
/// `Default` is [`Mode::Personal`] — the safest assumption for a
/// fresh `git clone` + `cargo run` on someone's laptop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Mode {
    /// No auth required; reads + writes are open to whoever holds
    /// the port. Login UI is hidden. Suitable for a laptop or a
    /// single-tenant container.
    #[default]
    Personal,
    /// Reads stay public; contributions and API-key management
    /// require an authenticated session. The shape consumed by the
    /// hosted instance.
    MultiUser,
}

impl Mode {
    /// Read [`MODE_ENV`] and parse it.
    ///
    /// - unset, or set to a string that trims to empty → `Ok(Mode::Personal)`
    /// - case-insensitive match for `personal` / `multi-user`
    ///   (with `multiuser` and `multi_user` aliases) → `Ok(_)`
    /// - any other value → `Err(ModeParseError)`
    ///
    /// Treating "set but blank" the same as "unset" mirrors the
    /// gateway's `non_empty_env` helper so a stray `MODE=` in a
    /// `.env` file does not bypass the default.
    pub fn from_env() -> Result<Self, ModeParseError> {
        match env::var(MODE_ENV) {
            Ok(raw) => {
                let trimmed = raw.trim();
                if trimmed.is_empty() {
                    Ok(Self::default())
                } else {
                    trimmed.parse()
                }
            }
            Err(env::VarError::NotPresent) => Ok(Self::default()),
            Err(env::VarError::NotUnicode(_)) => Err(ModeParseError {
                value: "<non-unicode>".to_owned(),
            }),
        }
    }

    /// Stable lowercase tag used in logs and config files.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Personal => "personal",
            Self::MultiUser => "multi-user",
        }
    }

    /// `true` when contribution + API-key endpoints require auth.
    /// Convenience for middleware that gates on this exact flag.
    #[must_use]
    pub const fn requires_auth(self) -> bool {
        matches!(self, Self::MultiUser)
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Mode {
    type Err = ModeParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "personal" => Ok(Self::Personal),
            "multi-user" | "multiuser" | "multi_user" => Ok(Self::MultiUser),
            other => Err(ModeParseError {
                value: other.to_owned(),
            }),
        }
    }
}

/// Returned when [`MODE_ENV`] is set to a value that is neither
/// `personal` nor `multi-user` (under any accepted alias).
#[derive(Debug, Error, PartialEq, Eq)]
#[error(
    "invalid MODE value {value:?}: expected `personal` or `multi-user` (aliases: multiuser, multi_user)"
)]
pub struct ModeParseError {
    /// The offending value, lowercased and trimmed.
    pub value: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_personal() {
        assert_eq!(Mode::default(), Mode::Personal);
    }

    #[test]
    fn parse_canonical_forms() {
        assert_eq!("personal".parse::<Mode>().unwrap(), Mode::Personal);
        assert_eq!("multi-user".parse::<Mode>().unwrap(), Mode::MultiUser);
    }

    #[test]
    fn parse_is_case_insensitive_and_trims() {
        assert_eq!("  Personal  ".parse::<Mode>().unwrap(), Mode::Personal);
        assert_eq!("MULTI-USER".parse::<Mode>().unwrap(), Mode::MultiUser);
    }

    #[test]
    fn parse_accepts_documented_aliases() {
        assert_eq!("multiuser".parse::<Mode>().unwrap(), Mode::MultiUser);
        assert_eq!("multi_user".parse::<Mode>().unwrap(), Mode::MultiUser);
    }

    #[test]
    fn parse_rejects_unknown_values() {
        let err = "single-user".parse::<Mode>().unwrap_err();
        assert_eq!(err.value, "single-user");
    }

    #[test]
    fn requires_auth_matches_mode() {
        assert!(!Mode::Personal.requires_auth());
        assert!(Mode::MultiUser.requires_auth());
    }

    #[test]
    fn display_matches_as_str() {
        assert_eq!(format!("{}", Mode::Personal), "personal");
        assert_eq!(format!("{}", Mode::MultiUser), "multi-user");
    }
}
