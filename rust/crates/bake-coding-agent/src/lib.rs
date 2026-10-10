//! Pi's coding-agent services for Bake's Rust agent.
//!
//! `bake-coding-agent` ports Pi's `packages/coding-agent` (release v1.1.0,
//! revision `abe508e1b89912adde45528136c3221eb69acdd7`, MIT; see the crate's
//! `NOTICE`). So far it holds the session store: [`session`] reads and
//! writes Pi's session format, and [`home`] places it under the Bake home
//! (D25, D32). No binary uses it yet.
//!
//! | Module | Source |
//! |---|---|
//! | [`session`] | Pi `src/core/session-manager.ts`, `session-cwd.ts`, `messages.ts`, `src/utils/paths.ts` |
//! | [`home`] | Bake: `scripts/release/bake`, `packages/util/home-paths` |

pub mod home;
pub mod session;
