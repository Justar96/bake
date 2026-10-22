//! Output and verdicts, kept free of Windows calls so they are tested on
//! every platform.
//!
//! Every outcome is one JSON object per line on stdout. A checked case is
//! one of three classes:
//!
//! - an `assertion`: a documented property the design relies on. If Windows
//!   contradicts it, the run fails.
//! - a `control`: the negative control for an assertion. It shows the same
//!   setup without the property under test behaves differently, so the
//!   assertion cannot pass for an unrelated reason. A contradicted control
//!   fails the run too.
//! - an `observation`: platform-dependent behavior the design does not rely
//!   on. It is recorded as observed and never fails the run.
//!
//! Separately, a harness `error` (a setup step, an FFI call the probe needs,
//! or a panic) and a `cleanup` failure each fail the run. A `skipped` case
//! names its reason and does not fail the run.

#![allow(dead_code)]

use std::fmt::Write as _;

/// Exit status bits: several can be set at once.
pub mod exit {
    pub const HARNESS_ERROR: i32 = 1;
    pub const CONTRADICTED: i32 = 2;
    pub const CLEANUP_FAILED: i32 = 4;
    /// Bad arguments, or the probe could not start.
    pub const USAGE: i32 = 64;
}

/// One JSON object, built field by field.
pub struct Line(String);

impl Line {
    pub fn new(event: &str) -> Self {
        let mut line = Line(String::from("{"));
        line.str("event", event);
        line
    }

    /// An object with no `event` field, for nesting with [`Line::raw`].
    pub fn object() -> Self {
        Line(String::from("{"))
    }

    fn key(&mut self, key: &str) {
        if self.0.len() > 1 {
            self.0.push(',');
        }
        push_string(&mut self.0, key);
        self.0.push(':');
    }

    pub fn str(&mut self, key: &str, value: &str) -> &mut Self {
        self.key(key);
        push_string(&mut self.0, value);
        self
    }

    pub fn num(&mut self, key: &str, value: impl std::fmt::Display) -> &mut Self {
        self.key(key);
        let _ = write!(self.0, "{value}");
        self
    }

    pub fn hex(&mut self, key: &str, value: u32) -> &mut Self {
        self.str(key, &format!("{value:#010x}"))
    }

    pub fn bool(&mut self, key: &str, value: bool) -> &mut Self {
        self.num(key, value)
    }

    /// Inserts `json`, which must already be valid JSON.
    pub fn raw(&mut self, key: &str, json: &str) -> &mut Self {
        self.key(key);
        self.0.push_str(json);
        self
    }

    pub fn finish(mut self) -> String {
        self.0.push('}');
        self.0
    }

    pub fn emit(self) {
        println!("{}", self.finish());
    }
}

/// Appends `value` as a JSON string literal.
pub fn push_string(out: &mut String, value: &str) {
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Assertion,
    Control,
    Observation,
}

impl Class {
    fn name(self) -> &'static str {
        match self {
            Class::Assertion => "assertion",
            Class::Control => "control",
            Class::Observation => "observation",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Pass,
    Contradicted,
    Observed,
    Skipped,
    Error,
}

impl Status {
    pub fn name(self) -> &'static str {
        match self {
            Status::Pass => "pass",
            Status::Contradicted => "contradicted",
            Status::Observed => "observed",
            Status::Skipped => "skipped",
            Status::Error => "error",
        }
    }
}

/// The status of a checked case: an observation is only observed; an
/// assertion or control passes exactly when `holds`.
pub fn status(class: Class, holds: bool) -> Status {
    match class {
        Class::Observation => Status::Observed,
        Class::Assertion | Class::Control if holds => Status::Pass,
        Class::Assertion | Class::Control => Status::Contradicted,
    }
}

/// Counts of every reported outcome; decides the exit status.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tally {
    pub pass: u32,
    pub contradicted: u32,
    pub observed: u32,
    pub skipped: u32,
    pub errors: u32,
    pub cleanup_failures: u32,
}

impl Tally {
    fn count(&mut self, status: Status) {
        match status {
            Status::Pass => self.pass += 1,
            Status::Contradicted => self.contradicted += 1,
            Status::Observed => self.observed += 1,
            Status::Skipped => self.skipped += 1,
            Status::Error => self.errors += 1,
        }
    }

    /// Reports one checked case. `expected` and `observed` are free text;
    /// `source` names the document the expectation comes from.
    #[allow(clippy::too_many_arguments)]
    pub fn check(
        &mut self,
        mechanism: &str,
        case: &str,
        class: Class,
        holds: bool,
        expected: &str,
        observed: &str,
        source: &str,
    ) -> Status {
        let status = status(class, holds);
        self.count(status);
        let mut line = Line::new("check");
        line.str("mechanism", mechanism)
            .str("case", case)
            .str("class", class.name())
            .str("status", status.name())
            .str("expected", expected)
            .str("observed", observed)
            .str("source", source);
        line.emit();
        status
    }

    pub fn skip(&mut self, mechanism: &str, case: &str, class: Class, reason: &str) {
        self.count(Status::Skipped);
        let mut line = Line::new("check");
        line.str("mechanism", mechanism)
            .str("case", case)
            .str("class", class.name())
            .str("status", Status::Skipped.name())
            .str("reason", reason);
        line.emit();
    }

    /// A harness error: the probe could not run a step it needs.
    pub fn error(&mut self, mechanism: &str, step: &str, detail: &str) {
        self.count(Status::Error);
        let mut line = Line::new("harness-error");
        line.str("mechanism", mechanism).str("step", step).str("detail", detail);
        line.emit();
    }

    pub fn cleanup_failure(&mut self, path: &str, detail: &str) {
        self.cleanup_failures += 1;
        let mut line = Line::new("cleanup-failure");
        line.str("path", path).str("detail", detail);
        line.emit();
    }

    pub fn exit_code(&self) -> i32 {
        let mut code = 0;
        if self.errors > 0 {
            code |= exit::HARNESS_ERROR;
        }
        if self.contradicted > 0 {
            code |= exit::CONTRADICTED;
        }
        if self.cleanup_failures > 0 {
            code |= exit::CLEANUP_FAILED;
        }
        code
    }

    pub fn summary(&self) -> String {
        let mut line = Line::new("summary");
        line.num("pass", self.pass)
            .num("contradicted", self.contradicted)
            .num("observed", self.observed)
            .num("skipped", self.skipped)
            .num("harnessErrors", self.errors)
            .num("cleanupFailures", self.cleanup_failures)
            .num("exitCode", self.exit_code());
        line.finish()
    }
}

/// A probe-root name: a fixed prefix, the process id, and 128 random bits
/// as hex. The name only avoids collisions; exclusive creation of the
/// directory is what proves this run owns it.
pub fn root_name(pid: u32, random: &[u8; 16]) -> String {
    let mut name = format!("bake-trust-probe-{pid}-");
    for byte in random {
        let _ = write!(name, "{byte:02x}");
    }
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_are_escaped() {
        let mut out = String::new();
        push_string(&mut out, "a\"b\\c\nd\u{1}é");
        assert_eq!(out, "\"a\\\"b\\\\c\\u000ad\\u0001é\"");
        let mut line = Line::new("x");
        line.str("path", r"C:\a").num("n", 3).bool("b", true).raw("o", "{}");
        assert_eq!(line.finish(), r#"{"event":"x","path":"C:\\a","n":3,"b":true,"o":{}}"#);
        assert_eq!(Line::object().finish(), "{}");
    }

    #[test]
    fn only_assertions_and_controls_can_be_contradicted() {
        assert_eq!(status(Class::Assertion, true), Status::Pass);
        assert_eq!(status(Class::Assertion, false), Status::Contradicted);
        assert_eq!(status(Class::Control, false), Status::Contradicted);
        assert_eq!(status(Class::Observation, false), Status::Observed);
        assert_eq!(status(Class::Observation, true), Status::Observed);
    }

    #[test]
    fn exit_codes_combine_every_failure_kind() {
        let mut tally = Tally::default();
        tally.check("m", "c", Class::Observation, false, "e", "o", "s");
        tally.skip("m", "c", Class::Assertion, "no privilege");
        assert_eq!(tally.exit_code(), 0);
        tally.check("m", "c", Class::Control, false, "e", "o", "s");
        assert_eq!(tally.exit_code(), exit::CONTRADICTED);
        tally.error("m", "setup", "boom");
        tally.cleanup_failure("p", "left");
        assert_eq!(tally.exit_code(), exit::CONTRADICTED | exit::HARNESS_ERROR | exit::CLEANUP_FAILED);
        assert_eq!(
            (tally.pass, tally.contradicted, tally.observed, tally.skipped, tally.errors, tally.cleanup_failures),
            (0, 1, 1, 1, 1, 1)
        );
        assert!(tally.summary().contains("\"exitCode\":7"));
    }

    #[test]
    fn root_names_carry_pid_and_all_random_bits() {
        let name = root_name(42, &[0xab; 16]);
        assert_eq!(name, format!("bake-trust-probe-42-{}", "ab".repeat(16)));
        assert_ne!(root_name(42, &[0; 16]), root_name(42, &[1; 16]));
    }
}
