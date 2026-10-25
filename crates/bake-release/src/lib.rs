//! Bake's release tool: it checks a tag, packs and checks the native
//! archive for this host, generates the third-party license bundle,
//! rehearses an install with the shipped installer, signs the native
//! manifest, and verifies what the download service serves.
//!
//! Every reading step uses `bake-update`, the code a user's `bake update`
//! runs, so a release is checked by the reader it is written for. The
//! release workflow (`.github/workflows/release.yml`) runs the steps;
//! `distribution/README.md` describes the route and the service.
//!
//! Ported from Bake's Bun release scripts (`scripts/release/*.ts` at
//! revision 97ce6abba0), which published 0.4.0, when the repository moved
//! to Cargo alone.

pub mod archive;
pub mod assemble;
pub mod glibc;
pub mod http;
pub mod licenses;
pub mod live;
pub mod manifest;
pub mod pack;
pub mod preflight;
pub mod rehearse;
pub mod util;
pub mod workspace;
