//! Runner-failure and denial classification, from
//! `packages/sandbox/sandbox/src/diagnostics.ts` and `RunnerFailureRule` in
//! `index.ts`.

/// Evidence that a sandbox runner failed before it ran the wrapped command.
/// A rule matches when the exit code is allowed (or no codes are listed),
/// and a stderr line, after the informational lines are removed by
/// case-insensitive exact equality, contains a fatal signature
/// case-insensitively. Exit status alone never proves a runner failure.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RunnerFailureRule {
    /// Nonzero exit codes the rule may match on; empty permits any nonzero exit.
    pub allowed_exit_codes: Vec<i32>,
    /// Substrings identifying a fatal runner diagnostic on one stderr line.
    pub fatal_signatures: Vec<String>,
    /// Benign stderr lines excluded by exact, case-insensitive equality.
    pub informational_lines: Vec<String>,
}

/// The first stderr line proving a runner failure, if any rule matches
/// (`classifyRunnerFailure`). `None` for a missing or zero exit code.
pub fn classify_runner_failure(
    exit_code: Option<i32>,
    stderr: &str,
    rules: &[RunnerFailureRule],
) -> Option<String> {
    let code = exit_code.filter(|code| *code != 0)?;
    for rule in rules {
        if !rule.allowed_exit_codes.is_empty() && !rule.allowed_exit_codes.contains(&code) {
            continue;
        }
        let informational: Vec<String> = rule
            .informational_lines
            .iter()
            .map(|line| line.to_lowercase())
            .collect();
        let fatal: Vec<String> = rule
            .fatal_signatures
            .iter()
            .filter(|signature| !signature.trim().is_empty())
            .map(|signature| signature.to_lowercase())
            .collect();
        for line in stderr.split('\n') {
            let line = line.strip_suffix('\r').unwrap_or(line);
            let lowered = line.to_lowercase();
            if informational.contains(&lowered) {
                continue;
            }
            if fatal.iter().any(|signature| lowered.contains(signature)) {
                return Some(line.to_owned());
            }
        }
    }
    None
}

/// Whether a failed run's stderr carries one of a backend's denial
/// signatures (`matchesSignature`). A missing or zero exit code never does.
pub fn matches_signature(exit_code: Option<i32>, stderr: &str, signatures: &[String]) -> bool {
    if exit_code.is_none_or(|code| code == 0) {
        return false;
    }
    let lowered = stderr.to_lowercase();
    signatures
        .iter()
        .any(|signature| lowered.contains(&signature.to_lowercase()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule() -> RunnerFailureRule {
        RunnerFailureRule {
            allowed_exit_codes: vec![1],
            fatal_signatures: vec!["bwrap: ".into(), "  ".into()],
            informational_lines: vec!["bwrap: note".into()],
        }
    }

    #[test]
    fn runner_failures_need_a_fatal_line() {
        assert_eq!(
            classify_runner_failure(Some(1), "bwrap: setting up uid map: denied\r\n", &[rule()])
                .as_deref(),
            Some("bwrap: setting up uid map: denied")
        );
        assert_eq!(
            classify_runner_failure(Some(1), "BWRAP: NOTE\n", &[rule()]),
            None
        );
        assert_eq!(
            classify_runner_failure(Some(2), "bwrap: x", &[rule()]),
            None
        );
        assert_eq!(
            classify_runner_failure(Some(0), "bwrap: x", &[rule()]),
            None
        );
        assert_eq!(classify_runner_failure(None, "bwrap: x", &[rule()]), None);
        assert_eq!(
            classify_runner_failure(Some(1), "plain error", &[rule()]),
            None
        );
    }

    #[test]
    fn denial_signatures_ignore_case_and_success() {
        let signatures = vec!["Read-only file system".to_owned()];
        assert!(matches_signature(
            Some(1),
            "touch: x: read-only FILE system",
            &signatures
        ));
        assert!(!matches_signature(
            Some(0),
            "read-only file system",
            &signatures
        ));
        assert!(!matches_signature(
            None,
            "read-only file system",
            &signatures
        ));
    }
}
