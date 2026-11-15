//! Bake's release tool: it checks a tag, packs and checks the native
//! archive for this host, generates the third-party license bundle,
//! rehearses an install with the shipped installer, signs the native
//! manifest, and verifies what the download service serves.
//!
//! Every reading step uses `bake-update`, the code a user's `bake update`
//! runs, so a release is checked by the reader it is written for.
//! [`prepare`] makes the release commit's changes and [`publish`] runs the
//! steps after the archives are packed; `distribution/README.md` describes
//! the steps, the route, and the service.

pub mod archive;
pub mod assemble;
pub mod glibc;
pub mod http;
pub mod licenses;
pub mod live;
pub mod manifest;
pub mod pack;
pub mod preflight;
pub mod prepare;
pub mod publish;
pub mod rehearse;
pub mod runner;
pub mod util;
pub mod workspace;
