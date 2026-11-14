//! `bake update`: the native updater.
//!
//! Pi has no updater, so this is Bake's own. It reads the signed native
//! release route ([`source`]), checks the manifest against the embedded
//! public key ([`manifest`]), unpacks a release beside the running one
//! ([`archive`]), runs the candidate's own self-check ([`check`]), and
//! moves the install's pointer atomically ([`layout`]) under the shared
//! install lock ([`lock`]). [`update::Updater`] is the whole run;
//! `bake-cli`'s `update` module is the command line over it.
//!
//! Nothing here panics on a manifest, an archive, an HTTP response, or a
//! file under the install root: every such failure is an [`UpdateError`].

pub mod archive;
pub mod check;
pub mod error;
pub mod layout;
pub mod lock;
pub mod manifest;
pub mod source;
pub mod update;
pub mod version;

pub use error::UpdateError;
pub use manifest::{Artifact, Manifest, RELEASE_TARGETS, TARGETS, host_target, release_target};
pub use update::{Effects, HostEffects, Outcome, Updater};
