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
//! | [`backends`] | the Linux bubblewrap and Landlock and macOS Seatbelt [`LocalProvider`], [`default_provider`], and the explicit-chain Windows runners |
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
//!
//! A [`SandboxPolicy`]'s workspace and extra roots are canonical paths the
//! caller resolved once, when the session started; the policy never resolves
//! them again, and the local backends refuse a call whose root has since
//! changed (see [`backends`]).
//!
//! # Known limits
//!
//! The modes govern **file writes made by the confined process tree**. They
//! do not stop that tree from asking another process, one outside the
//! sandbox, to write for it. Every backend leaves the network namespace and
//! Unix-domain sockets reachable, as the TypeScript profiles did
//! (`profiles.ts`), so a `read-only` command can still cause writes through:
//!
//! - **The user's systemd manager over D-Bus.** The session bus socket
//!   `/run/user/$UID/bus` stays visible and connectable: bubblewrap binds the
//!   whole tree read-only, and a read-only bind does not stop `connect(2)` on
//!   a socket file; Landlock's filesystem rules (ABI 1 to 5) do not govern
//!   connecting to an existing socket at all. `systemd-run --user --wait
//!   touch ~/x` therefore runs an unconfined command as the user, and so
//!   does any D-Bus service that writes files on request.
//! - **Credential and control agents.** `ssh-agent` (`$SSH_AUTH_SOCK`) and
//!   `gpg-agent` sign and decrypt with the user's keys; the Docker daemon
//!   (`/var/run/docker.sock`, for members of the `docker` group) can start a
//!   container that binds the host filesystem read-write; a terminal
//!   multiplexer's socket (`tmux send-keys`) types into the user's shells.
//! - **Abstract-namespace Unix sockets and the network.** Abstract sockets
//!   have no file at all, and no backend unshares the network, so loopback
//!   services with write endpoints are reachable too.
//! - **macOS.** The Seatbelt profile is `(allow default)` with file writes
//!   denied, so Mach and XPC lookups (`launchctl`, for one) and Unix
//!   sockets remain allowed. `sandbox-exec` also does not start a new
//!   terminal session.
//!
//! The enforcement each backend reports is TypeScript's and does not account
//! for these channels: [`Enforcement::Full`] means the file rules are fully
//! enforced, not that no write can happen on the command's behalf. Closing
//! them (hiding `/run/user/$UID` and agent sockets, unsharing the network,
//! Landlock ABI 6 socket scoping, a seccomp filter) is an open decision for
//! the support register.
#![warn(missing_docs)]

pub mod backends;
pub mod diagnostics;
pub mod escalation;
pub mod mode;
pub mod paths;
pub mod policy;
pub mod provider;
#[cfg(any(test, feature = "testing"))]
pub mod testing;

pub use backends::{LocalProvider, default_provider};
pub use diagnostics::{RunnerFailureRule, classify_runner_failure, matches_signature};
pub use escalation::{ESCALATION_TARGETS, EscalationOutcome};
pub use mode::{SandboxMode, UnknownSandboxMode};
pub use paths::{canonical_path, is_path_under};
pub use policy::SandboxPolicy;
pub use provider::{
    ConfineFuture, ConfinedCommand, Enforcement, ProbeOwner, SandboxProvider, SandboxUnavailable,
    UnavailableProvider,
};
