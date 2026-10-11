//! Bake's process-confinement contract (D24).
//!
//! The retained Bake sandbox, ported from the TypeScript packages
//! `packages/sandbox/sandbox` (`index.ts`, `escalation.ts`, `roots.ts`,
//! `diagnostics.ts`) and the write containment of `packages/fs/fs-sandbox`.
//! Pi has no sandbox; this crate is the seam Pi's tools are wrapped through.
//!
//! | Module | Contents |
//! |---|---|
//! | [`mode`] | [`SandboxMode`] and the strict-widening order between modes |
//! | [`policy`] | [`SandboxPolicy`], the complete per-call file-effect policy, and its writable roots |
//! | [`paths`] | canonical paths and containment, shared by enforcement and the file tools |
//! | [`escalation`] | the model-visible escalation vocabulary: targets, argument checks, markers, outcomes |
//! | [`diagnostics`] | runner-failure and denial classification from a confined run's stderr |
//! | [`provider`] | [`SandboxProvider`], [`ConfinedCommand`], and the fail-closed [`UnavailableProvider`] |
//!
//! # Contract
//!
//! A provider wraps an exact argv so it runs confined under one
//! [`SandboxPolicy`], or refuses with [`SandboxUnavailable`]. It never returns
//! the caller's argv unconfined: a missing backend fails closed, and a policy
//! the backend can enforce only in part is reported as
//! [`Enforcement::Partial`]. Policies travel per call, so two calls may run
//! under different modes at once and an approved escalation is simply a new
//! call with a wider policy.
//!
//! The types are `#[non_exhaustive]` where later policy dimensions (network,
//! read restrictions, extra roots from rules) are expected to grow, so adding
//! one does not break the tools and backends built on this contract.
#![warn(missing_docs)]

pub mod diagnostics;
pub mod escalation;
pub mod mode;
pub mod paths;
pub mod policy;
pub mod provider;
#[cfg(any(test, feature = "testing"))]
pub mod testing;

pub use diagnostics::{RunnerFailureRule, classify_runner_failure, matches_signature};
pub use escalation::{ESCALATION_TARGETS, EscalationOutcome};
pub use mode::{SandboxMode, UnknownSandboxMode};
pub use paths::{canonical_path, is_path_under};
pub use policy::SandboxPolicy;
pub use provider::{
    ConfineFuture, ConfinedCommand, Enforcement, SandboxProvider, SandboxUnavailable,
    UnavailableProvider,
};
