//! The command line's plan ([`plan`]): what a parsed invocation does, its
//! flag checks, and the initial message.

use super::*;

/// Writes Pi's diagnostic lines.
pub(super) fn report(stderr: &SharedWriter, diagnostics: &[Diagnostic]) {
    for diagnostic in diagnostics {
        let prefix = match diagnostic.kind {
            DiagnosticKind::Error => "Error",
            DiagnosticKind::Warning => "Warning",
        };
        write_out(stderr, &format!("{prefix}: {}\n", diagnostic.message));
    }
}

pub(super) fn error(stderr: &SharedWriter, message: &str) -> i32 {
    write_out(stderr, &format!("Error: {message}\n"));
    1
}

pub(super) fn warn(stderr: &SharedWriter, message: &str) {
    write_out(stderr, &format!("Warning: {message}\n"));
}

/// Pi's argument checks and `resolveAppMode`, before anything is loaded:
/// print mode when asked for or when a standard stream is not a terminal,
/// and interactive mode otherwise.
pub fn plan(
    args: &[String],
    stdin_is_terminal: bool,
    stdout_is_terminal: bool,
    stderr: &SharedWriter,
) -> Action {
    let mut parsed = parse_args(args);
    report(stderr, &parsed.diagnostics);
    if parsed
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.kind == DiagnosticKind::Error)
    {
        return Action::Exit(1);
    }
    if parsed.version {
        return Action::Version;
    }
    // Pi's `main` exports before it reads any other flag, `--help`
    // included, so `--export` with anything else still exports.
    if let Some(input) = parsed.export.take() {
        return Action::Export(input, parsed.messages.first().cloned());
    }
    if parsed.help {
        return Action::Help;
    }
    if let Some(message) = unknown_flags_message(&parsed.unknown_flags) {
        return Action::Exit(error(stderr, &message));
    }
    if parsed.project_trust_override.is_some() {
        return Action::Exit(error(stderr, PROJECT_TRUST_REFUSED));
    }
    if let Some(message) = validate_fork_flags(&parsed) {
        return Action::Exit(error(stderr, &message));
    }
    if let Some(message) = validate_session_id_flags(&parsed) {
        return Action::Exit(error(stderr, &message));
    }
    if let Some(message) = validate_run_recovery_flags(&parsed) {
        return Action::Exit(error(stderr, &message));
    }
    if let Some(name) = &parsed.name
        && normalize_session_name(name).is_none()
    {
        return Action::Exit(error(stderr, "--name requires a non-empty value"));
    }
    if let Some(format) = parsed.output_format {
        if parsed.mode.is_some() {
            return Action::Exit(error(
                stderr,
                "--output-format and --mode choose the same thing; give one of them",
            ));
        }
        parsed.mode = Some(format);
    }
    if parsed.mode == Some(Mode::Rpc) {
        if let Some(message) = validate_rpc_flags(&parsed) {
            return Action::Exit(error(stderr, &message));
        }
        return Action::Rpc(Box::new(parsed));
    }
    let mode = match parsed.mode {
        Some(mode) => mode,
        None if parsed.print
            || parsed.read_stdin
            || parsed.list_models.is_some()
            || !stdin_is_terminal
            || !stdout_is_terminal =>
        {
            Mode::Text
        }
        None => {
            if parsed.recoverable || parsed.resume_run.is_some() {
                return Action::Exit(error(
                    stderr,
                    "--recoverable and --resume-run require print mode (--print or --mode)",
                ));
            }
            if parsed.max_model_calls.is_some() || parsed.run_timeout_ms.is_some() {
                return Action::Exit(error(
                    stderr,
                    "--max-model-calls and --run-timeout-ms require print mode (--print or --mode)",
                ));
            }
            return Action::Interactive(Box::new(parsed));
        }
    };
    // Pi's `--resume` opens a terminal session picker from
    // `createSessionManager`, whatever the mode; print mode here has no
    // terminal to open it in, so it is refused instead of ignored.
    if resumes_at_startup(&parsed) {
        return Action::Exit(error(
            stderr,
            "--resume opens the session picker, which print mode has no terminal for; use --session or --continue",
        ));
    }
    Action::Print(Box::new(parsed), mode)
}

/// The flags RPC mode cannot serve: the print-mode run limits, which it
/// takes no prompt to spend, and Pi's refusal of `@file` arguments, since
/// standard input carries the protocol rather than a prompt.
fn validate_rpc_flags(args: &Args) -> Option<String> {
    if resumes_at_startup(args) {
        return Some(
            "--resume opens the session picker, which RPC mode has no terminal for; use --session or --continue"
                .to_owned(),
        );
    }
    if !args.file_args.is_empty() {
        return Some("@file arguments are not supported in RPC mode".into());
    }
    if args.read_stdin {
        return Some(
            "- reads a prompt from standard input, which carries the protocol in RPC mode".into(),
        );
    }
    if args.recoverable || args.resume_run.is_some() {
        return Some(
            "--recoverable and --resume-run require print mode (--print or --mode text or json)"
                .into(),
        );
    }
    if args.max_model_calls.is_some() || args.run_timeout_ms.is_some() {
        return Some(
            "--max-model-calls and --run-timeout-ms require print mode (--print or --mode text or json)"
                .into(),
        );
    }
    if args.list_models.is_some() {
        return Some("--list-models cannot be combined with --mode rpc".into());
    }
    None
}

fn validate_run_recovery_flags(args: &Args) -> Option<String> {
    if !args.recoverable && args.resume_run.is_none() {
        return None;
    }
    if args.no_session {
        return Some("Run recovery requires a persisted session; remove --no-session.".into());
    }
    if args.recoverable && args.resume_run.is_some() {
        return Some("--recoverable and --resume-run cannot be combined.".into());
    }
    if args.resume_run.is_some() {
        if !args.session.as_deref().is_some_and(is_session_path) {
            return Some(
                "--resume-run requires --session with an explicit session file path.".into(),
            );
        }
        if args.continue_session || args.fork.is_some() || args.session_id.is_some() {
            return Some(
                "--resume-run cannot be combined with --continue, --fork, or --session-id.".into(),
            );
        }
        if args.max_model_calls.is_some() || args.run_timeout_ms.is_some() {
            return Some(
                "--resume-run restores the recorded budget; new run limits cannot replace it."
                    .into(),
            );
        }
        if !args.messages.is_empty() || !args.file_args.is_empty() || args.read_stdin {
            return Some("--resume-run takes no new prompt or file arguments.".into());
        }
    } else if args.messages.len() > 1 {
        return Some("--recoverable accepts one prompt per invocation.".into());
    }
    None
}

/// Pi's `validateForkFlags`.
fn validate_fork_flags(parsed: &Args) -> Option<String> {
    parsed.fork.as_ref()?;
    let mut conflicts = Vec::new();
    if parsed.session.is_some() {
        conflicts.push("--session");
    }
    if parsed.continue_session {
        conflicts.push("--continue");
    }
    if parsed.resume {
        conflicts.push("--resume");
    }
    if parsed.no_session {
        conflicts.push("--no-session");
    }
    (!conflicts.is_empty())
        .then(|| format!("--fork cannot be combined with {}", conflicts.join(", ")))
}

/// Pi's `validateSessionIdFlags`.
fn validate_session_id_flags(parsed: &Args) -> Option<String> {
    let session_id = parsed.session_id.as_ref()?;
    let mut conflicts = Vec::new();
    if parsed.session.is_some() {
        conflicts.push("--session");
    }
    if parsed.continue_session {
        conflicts.push("--continue");
    }
    if parsed.resume {
        conflicts.push("--resume");
    }
    if !conflicts.is_empty() {
        return Some(format!(
            "--session-id cannot be combined with {}",
            conflicts.join(", ")
        ));
    }
    assert_valid_session_id(session_id)
        .err()
        .map(|error| error.to_string())
}

/// Pi's `buildInitialMessage`: stdin, `@file` text, then the first CLI
/// message; later CLI messages stay separate prompts.
pub(super) fn build_initial_message(
    stdin: Option<String>,
    file_text: &str,
    messages: &mut Vec<String>,
) -> Option<String> {
    let mut parts: Vec<String> = stdin.into_iter().collect();
    if !file_text.is_empty() {
        parts.push(file_text.to_owned());
    }
    if !messages.is_empty() {
        parts.push(messages.remove(0));
    }
    (!parts.is_empty()).then(|| parts.concat())
}
