//! `bake update`: the native 0.4 updater.
//!
//! Pi has no updater, so this is Bake's own. It reads the signed native
//! release route ([`source`]), checks the manifest against the embedded
//! public key ([`manifest`]), unpacks a release beside the running one
//! ([`archive`]), runs the candidate's own self-check ([`check`]), and
//! moves the install's pointer atomically ([`layout`]) under the shared
//! install lock ([`lock`]). [`update::Updater`] is the whole run;
//! `bake-cli`'s `update` module is the command line over it.
//!
//! Bake 0.3's TypeScript updater (`packages/boot/updater/src/`) is the
//! reference for the manifest rules, the version ordering, the streamed
//! and hashed download, and the atomic pointer move. It is not a
//! compatibility target: the native route, manifest schema, install
//! layout, and key are separate from 0.3's, and neither updater can read
//! the other's manifest ([D14] and [D45]).
//!
//! [D14]: ../../../docs/roadmap/rust-0.4/scope-00/support.md
//! [D45]: ../../../docs/roadmap/rust-0.4/scope-00/support.md
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
