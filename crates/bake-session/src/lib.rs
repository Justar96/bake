//! Pi's session store for Bake, and the files beside it.
//!
//! `bake-session` holds what `bake-coding-agent` reads and writes on disk
//! without an agent: [`session`], Pi's session format and its manager, with
//! Bake's writer locks, synced writes, and recovery; [`home`], the Bake home
//! the default session directories live under; and [`json_store`], the
//! locked, atomically replaced JSON files behind `settings.json` and
//! `auth.json`. It depends on no Bake crate above `bake-agent`, so these
//! contracts have their own test run.
//!
//! `bake-coding-agent` re-exports the three modules under their old paths
//! (`bake_coding_agent::session`, `::home`, `::json_store`).
//!
//! | Module | Source |
//! |---|---|
//! | [`session`] | Pi `src/core/session-manager.ts`, `session-cwd.ts`, `messages.ts`, `src/utils/paths.ts` (v1.1.0); session search and `packages/tui/src/fuzzy.ts` (v1.2.0) |
//! | [`home`] | Bake: the Bake home (`BAKE_HOME`, else `~/.bake`) |
//! | [`json_store`] | Pi `src/core/auth-storage.ts` `FileAuthStorageBackend`, `src/core/settings-manager.ts` `FileSettingsStorage` (v1.2.0) |
//!
//! The `testing` feature exposes fault injection and crash-test helpers to
//! the consumers' tests; only dev-dependencies enable it.

pub mod home;
pub mod json_store;
pub mod session;
#[cfg(any(test, feature = "testing"))]
pub mod test_support;
