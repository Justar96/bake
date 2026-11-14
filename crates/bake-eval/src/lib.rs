//! Bake's paired evaluation runner.
//!
//! A change to what a model reads or to how the agent loop runs is measured
//! by running two built `bake` executables, the base and the candidate, on
//! the same tasks, models, and trial numbers, and comparing them pair by
//! pair. This crate is that runner; it is a build tool and never shipped.
//!
//! - A [`task`] is a directory: a workspace template, a prompt, and a script
//!   that decides whether the workspace and the answer pass.
//! - A [`sample`] is one `bake -p --output-format stream-json` run of one
//!   task on one model by one executable, in a directory of its own with a
//!   home of its own, followed by the task's check.
//! - [`run`] schedules the samples, keeps their records, and writes the
//!   [`report`]: per model, the paired change of each measure with a
//!   bootstrap interval ([`stats`]).
//!
//! Isolation: a sample's `HOME` and `BAKE_HOME` are inside its directory, so
//! the user's settings, sessions, skills, and context files are neither
//! read nor written. The one thing taken from the user's Bake home is the
//! provider account ([`home`]), copied into the sample's home and removed
//! when the sample ends.
//!
//! Tasks are shell and Python scripts, so evaluations run on Unix hosts.

pub mod cli;
pub mod home;
pub mod process;
pub mod report;
pub mod run;
pub mod sample;
pub mod stats;
pub mod stream;
pub mod task;
