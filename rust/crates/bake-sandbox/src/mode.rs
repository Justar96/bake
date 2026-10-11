//! Sandbox modes, from `SandboxMode` and `WIDER_MODES` in
//! `packages/sandbox/sandbox/src/{index,escalation}.ts`.

use std::fmt;
use std::str::FromStr;

/// The file-effect policy for confined processes. `read-only` permits only
/// required sinks such as `/dev/null`; `workspace-write` also permits the
/// workspace and the temporary directories; `danger-full-access` bypasses
/// confinement. Network and process visibility are outside this vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SandboxMode {
    /// No writes outside required sinks.
    ReadOnly,
    /// Writes inside the workspace and temporary directories.
    WorkspaceWrite,
    /// No confinement.
    DangerFullAccess,
}

impl SandboxMode {
    /// Every mode, narrowest first.
    pub const ALL: [SandboxMode; 3] = [
        SandboxMode::ReadOnly,
        SandboxMode::WorkspaceWrite,
        SandboxMode::DangerFullAccess,
    ];

    /// The wire name, as the TypeScript runtime and the model see it.
    pub fn as_str(self) -> &'static str {
        match self {
            SandboxMode::ReadOnly => "read-only",
            SandboxMode::WorkspaceWrite => "workspace-write",
            SandboxMode::DangerFullAccess => "danger-full-access",
        }
    }

    /// Whether processes under this mode are confined at all.
    pub fn is_confined(self) -> bool {
        self != SandboxMode::DangerFullAccess
    }

    /// The modes strictly wider than this one, narrowest first (`WIDER_MODES`).
    pub fn wider_modes(self) -> &'static [SandboxMode] {
        match self {
            SandboxMode::ReadOnly => &[SandboxMode::WorkspaceWrite, SandboxMode::DangerFullAccess],
            SandboxMode::WorkspaceWrite => &[SandboxMode::DangerFullAccess],
            SandboxMode::DangerFullAccess => &[],
        }
    }

    /// Whether `other` grants strictly more than this mode.
    pub fn is_widened_by(self, other: SandboxMode) -> bool {
        self.wider_modes().contains(&other)
    }
}

impl fmt::Display for SandboxMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A string that names no [`SandboxMode`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownSandboxMode(pub String);

impl fmt::Display for UnknownSandboxMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown sandbox mode \"{}\"", self.0)
    }
}

impl std::error::Error for UnknownSandboxMode {}

impl FromStr for SandboxMode {
    type Err = UnknownSandboxMode;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        SandboxMode::ALL
            .into_iter()
            .find(|mode| mode.as_str() == value)
            .ok_or_else(|| UnknownSandboxMode(value.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for mode in SandboxMode::ALL {
            assert_eq!(mode.as_str().parse::<SandboxMode>(), Ok(mode));
        }
        assert!("Read-Only".parse::<SandboxMode>().is_err());
        assert!("".parse::<SandboxMode>().is_err());
    }

    #[test]
    fn widening_is_strict_and_matches_typescript() {
        use SandboxMode::*;
        assert!(ReadOnly.is_widened_by(WorkspaceWrite));
        assert!(ReadOnly.is_widened_by(DangerFullAccess));
        assert!(WorkspaceWrite.is_widened_by(DangerFullAccess));
        for mode in SandboxMode::ALL {
            assert!(!mode.is_widened_by(mode));
            assert!(!mode.is_widened_by(ReadOnly));
        }
        assert!(DangerFullAccess.wider_modes().is_empty());
    }
}
