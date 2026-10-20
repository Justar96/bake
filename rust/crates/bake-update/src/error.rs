//! What an update run reports when it stops.

use std::fmt;

/// Why an update, check, or rollback stopped. Every variant carries the
/// plain sentence the command prints; none carries a cause chain, so a
/// message never leaks a path or a library's wording the user cannot act
/// on. Bake 0.3's `UpdateError` (`packages/boot/updater/src/manifest.ts`)
/// is one untyped class; these are its failures named apart, so a caller
/// can tell a refused manifest from a held lock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateError {
    /// The manifest is not the JSON a native release manifest must be.
    Manifest(String),
    /// The manifest's signature is missing, malformed, or not this key's.
    Signature(String),
    /// The release host could not be reached, or answered with an error.
    Network(String),
    /// The archive's size or hash is not the one the manifest states.
    Artifact(String),
    /// The archive could not be unpacked, or holds a member that is refused.
    Archive(String),
    /// The manifest's version cannot replace the installed one.
    Version(String),
    /// The candidate release did not pass its own self-check.
    SelfCheck(String),
    /// Another install, update, rollback, or prune holds `install.lock`.
    Locked(String),
    /// This executable is not part of a native install, or the install's
    /// files are not the ones an install has.
    NotInstalled(String),
    /// A file under the install root could not be read or written.
    Install(String),
}

impl UpdateError {
    /// The sentence the command prints, without a prefix.
    pub fn message(&self) -> &str {
        match self {
            Self::Manifest(message)
            | Self::Signature(message)
            | Self::Network(message)
            | Self::Artifact(message)
            | Self::Archive(message)
            | Self::Version(message)
            | Self::SelfCheck(message)
            | Self::Locked(message)
            | Self::NotInstalled(message)
            | Self::Install(message) => message,
        }
    }
}

impl fmt::Display for UpdateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message())
    }
}

impl std::error::Error for UpdateError {}
