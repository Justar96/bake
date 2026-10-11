//! Pi's coding-agent services for Bake's Rust agent.
//!
//! `bake-coding-agent` ports Pi's `packages/coding-agent` (release v1.1.0,
//! revision `abe508e1b89912adde45528136c3221eb69acdd7`, MIT; see the crate's
//! `NOTICE`). [`session`] reads and writes Pi's session format, [`home`]
//! places it under the Bake home (D25, D32), and [`tools`] provides output
//! truncation and accumulation. No binary uses it yet.
//!
//! | Module | Source |
//! |---|---|
//! | [`session`] | Pi `src/core/session-manager.ts`, `session-cwd.ts`, `messages.ts`, `src/utils/paths.ts` |
//! | [`tools`] | Pi `src/core/tools/{truncate,output-accumulator}.ts`, `src/utils/output-files.ts` |
//! | [`home`] | Bake: `scripts/release/bake`, `packages/util/home-paths` |
//! | [`cliproxyapi`] | Bake: `apps/tui/packages/app/src/cliproxyapi.ts`, the retained CLIProxyAPI route and its read-only D25 import |

pub mod cliproxyapi;
pub mod home;
pub mod session;
pub mod tools;
