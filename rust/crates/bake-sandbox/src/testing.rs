//! Test doubles for code above the seam. Enabled by the `testing` feature;
//! never compose them into a shipped binary.

use std::ffi::OsString;
use std::sync::Mutex;

use crate::mode::SandboxMode;
use crate::policy::SandboxPolicy;
use crate::provider::{ConfineFuture, ConfinedCommand, Enforcement, SandboxProvider};

/// Runs the caller's argv unchanged (no confinement) and records each call's
/// argv and policy. Use it to test tools and policy layers; real enforcement
/// is tested against real backends.
#[derive(Debug, Default)]
pub struct RecordingProvider {
    calls: Mutex<Vec<(Vec<OsString>, SandboxPolicy)>>,
    denial_signatures: Vec<String>,
}

impl RecordingProvider {
    /// A provider whose wraps report `signatures` as their denial dialect.
    pub fn with_denial_signatures(signatures: impl IntoIterator<Item = String>) -> Self {
        Self {
            calls: Mutex::default(),
            denial_signatures: signatures.into_iter().collect(),
        }
    }

    /// Every call so far: its argv and policy.
    pub fn calls(&self) -> Vec<(Vec<OsString>, SandboxPolicy)> {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// The modes of the calls so far.
    pub fn modes(&self) -> Vec<SandboxMode> {
        self.calls()
            .into_iter()
            .map(|(_, policy)| policy.mode)
            .collect()
    }
}

impl SandboxProvider for RecordingProvider {
    fn name(&self) -> &str {
        "recording"
    }

    fn confine<'a>(&'a self, argv: &'a [OsString], policy: &'a SandboxPolicy) -> ConfineFuture<'a> {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((argv.to_vec(), policy.clone()));
        let mut parts = argv.iter().cloned();
        let program = parts.next().unwrap_or_default();
        let command = ConfinedCommand::new(program, parts.collect(), Enforcement::Full)
            .with_denial_signatures(self.denial_signatures.clone());
        Box::pin(async move { Ok(command) })
    }
}
