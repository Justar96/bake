//! Parsing a Claude Code or Codex `hooks.json`, ported from Bake 0.3's
//! `packages/hooks/hooks-claude-code/src/config.ts` and
//! `packages/hooks/hooks-codex/src/config.ts`.
//!
//! Both read a settings object whose `hooks` member holds the event map,
//! or the bare event map. Only the five events [`HookEvent`] names run;
//! other events are ignored before their groups are read, so they cannot
//! invalidate the file, except that Claude Code's `SubagentStart` and
//! `SubagentStop`, which 0.3 ran, are reported as not run. Malformed
//! entries are dropped; non-command hooks, and Codex's `async: true`
//! hooks, are skipped with a warning; an invalid matcher on a runnable
//! group rejects the whole file. `UserPromptSubmit` and `Stop` have no
//! matcher subject, so their matchers are discarded before validation.
//!
//! Not ported: 0.3's `${CLAUDE_PLUGIN_ROOT}` and `${CLAUDE_PROJECT_DIR}`
//! substitution, which needed a configured root. The shell expands
//! `${CLAUDE_PROJECT_DIR}` from the variable every Claude Code hook gets.

use std::time::Duration;

use serde_json::{Map, Value};

use super::matcher::matcher_diagnostic;
use super::{HookDialect, HookEvent};

/// The longest per-hook timeout honored; a longer one is cut to it.
const MAX_TIMEOUT: Duration = Duration::from_secs(86_400);

/// One `{ "type": "command", "command": …, "timeout": … }` hook.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandHook {
    /// The shell command line.
    pub command: String,
    /// Its own timeout (`timeout` in seconds; Codex also `timeoutSec`).
    pub timeout: Option<Duration>,
}

/// One matcher group of an event.
#[derive(Debug, Clone, PartialEq)]
pub struct MatcherGroup {
    /// The pattern; `None` matches everything.
    pub matcher: Option<String>,
    /// Its command hooks, in file order.
    pub hooks: Vec<CommandHook>,
}

/// The runnable hooks of one file, by event, in file order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HookGroups {
    groups: Vec<(HookEvent, Vec<MatcherGroup>)>,
}

impl HookGroups {
    /// The groups of `event`.
    pub fn of(&self, event: HookEvent) -> &[MatcherGroup] {
        self.groups
            .iter()
            .find(|(each, _)| *each == event)
            .map_or(&[], |(_, groups)| groups.as_slice())
    }

    /// Whether no event has a hook.
    pub fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }
}

/// A parsed file: its runnable hooks and a warning for each hook skipped.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParsedHooks {
    pub groups: HookGroups,
    pub skipped: Vec<String>,
}

fn timeout(hook: &Map<String, Value>, dialect: HookDialect) -> Option<Duration> {
    let value = hook.get("timeout").and_then(Value::as_f64).or_else(|| {
        (dialect == HookDialect::Codex)
            .then(|| hook.get("timeoutSec").and_then(Value::as_f64))
            .flatten()
    })?;
    (value.is_finite() && value > 0.0)
        .then(|| Duration::from_secs_f64(value.min(MAX_TIMEOUT.as_secs_f64())))
}

/// Parses `raw` as `dialect`'s hook configuration; `Err` names an invalid
/// matcher and its event, and the whole file is then not used.
pub fn parse_hooks(raw: &Value, dialect: HookDialect) -> Result<ParsedHooks, String> {
    let mut parsed = ParsedHooks::default();
    let Some(root) = raw.as_object() else {
        return Ok(parsed);
    };
    let events = root.get("hooks").and_then(Value::as_object).unwrap_or(root);
    if dialect == HookDialect::ClaudeCode {
        for event in ["SubagentStart", "SubagentStop"] {
            if events.get(event).is_some_and(Value::is_array) {
                parsed
                    .skipped
                    .push(format!("{event} hooks are not run by bake"));
            }
        }
    }
    for event in HookEvent::ALL {
        let Some(raw_groups) = events.get(event.as_str()).and_then(Value::as_array) else {
            continue;
        };
        let mut groups = Vec::new();
        for raw_group in raw_groups {
            let Some(group) = raw_group.as_object() else {
                continue;
            };
            let Some(raw_hooks) = group.get("hooks").and_then(Value::as_array) else {
                continue;
            };
            let mut hooks = Vec::new();
            for raw_hook in raw_hooks {
                let Some(hook) = raw_hook.as_object() else {
                    continue;
                };
                let kind = hook
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("command");
                if kind != "command" {
                    parsed.skipped.push(match dialect {
                        HookDialect::ClaudeCode => format!(
                            "skipping unsupported \"{kind}\" hook on {} (only command hooks run)",
                            event.as_str()
                        ),
                        HookDialect::Codex => format!(
                            "skipping unsupported \"{kind}\" hook on {} (only sync command hooks run)",
                            event.as_str()
                        ),
                    });
                    continue;
                }
                if dialect == HookDialect::Codex && hook.get("async") == Some(&Value::Bool(true)) {
                    parsed.skipped.push(format!(
                        "skipping async hook on {} (only sync command hooks run)",
                        event.as_str()
                    ));
                    continue;
                }
                let Some(command) = hook.get("command").and_then(Value::as_str) else {
                    continue;
                };
                hooks.push(CommandHook {
                    command: command.to_owned(),
                    timeout: timeout(hook, dialect),
                });
            }
            if hooks.is_empty() {
                continue;
            }
            let matcher = if event.has_matcher() {
                group
                    .get("matcher")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            } else {
                None
            };
            if let Some(diagnostic) = matcher_diagnostic(matcher.as_deref(), dialect) {
                return Err(format!(
                    "{diagnostic} on event {}",
                    Value::from(event.as_str())
                ));
            }
            groups.push(MatcherGroup { matcher, hooks });
        }
        if !groups.is_empty() {
            parsed.groups.groups.push((event, groups));
        }
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    //! Ported from 0.3's `hooks-claude-code/tests/config.spec.ts` and
    //! `hooks-codex/tests/config.spec.ts`.
    use serde_json::json;

    use super::*;

    const CLAUDE: HookDialect = HookDialect::ClaudeCode;
    const CODEX: HookDialect = HookDialect::Codex;

    fn hook(command: &str, seconds: Option<u64>) -> CommandHook {
        CommandHook {
            command: command.into(),
            timeout: seconds.map(Duration::from_secs),
        }
    }

    fn group(matcher: Option<&str>, hooks: Vec<CommandHook>) -> MatcherGroup {
        MatcherGroup {
            matcher: matcher.map(str::to_owned),
            hooks,
        }
    }

    fn parse(raw: Value, dialect: HookDialect) -> ParsedHooks {
        parse_hooks(&raw, dialect).expect("valid")
    }

    #[test]
    fn a_bare_map_and_a_settings_wrapper_parse_alike() {
        for dialect in [CLAUDE, CODEX] {
            let groups = json!({ "PreToolUse": [{ "matcher": "Bash", "hooks": [{ "type": "command", "command": "x.sh" }] }] });
            let bare = parse(groups.clone(), dialect);
            assert_eq!(bare, parse(json!({ "hooks": groups }), dialect));
            assert_eq!(
                bare.groups.of(HookEvent::PreToolUse),
                [group(Some("Bash"), vec![hook("x.sh", None)])]
            );
        }
    }

    #[test]
    fn timeouts_are_seconds_and_codex_takes_the_timeout_sec_alias() {
        let parsed = parse(
            json!({
                "Stop": [{ "hooks": [{ "type": "command", "command": "${NOT_SUBSTITUTED}/s.sh", "timeout": 10 }] }],
                "UserPromptSubmit": [{ "hooks": [{ "type": "command", "command": "u.sh", "timeoutSec": 20 }] }],
            }),
            CODEX,
        );
        assert_eq!(
            parsed.groups.of(HookEvent::Stop),
            [group(None, vec![hook("${NOT_SUBSTITUTED}/s.sh", Some(10))])]
        );
        assert_eq!(
            parsed.groups.of(HookEvent::UserPromptSubmit),
            [group(None, vec![hook("u.sh", Some(20))])]
        );
        // Claude Code has no alias.
        let parsed = parse(
            json!({ "Stop": [{ "hooks": [{ "command": "u.sh", "timeoutSec": 20 }] }] }),
            CLAUDE,
        );
        assert_eq!(
            parsed.groups.of(HookEvent::Stop),
            [group(None, vec![hook("u.sh", None)])]
        );
    }

    #[test]
    fn non_command_hooks_are_skipped_and_their_siblings_kept() {
        let parsed = parse(
            json!({ "PreToolUse": [{ "hooks": [
                { "type": "prompt", "prompt": "hi" },
                { "type": "command", "command": "ok.sh" },
                { "type": "http", "url": "http://x" },
            ] }] }),
            CLAUDE,
        );
        assert_eq!(
            parsed.groups.of(HookEvent::PreToolUse),
            [group(None, vec![hook("ok.sh", None)])]
        );
        assert_eq!(
            parsed.skipped,
            [
                "skipping unsupported \"prompt\" hook on PreToolUse (only command hooks run)",
                "skipping unsupported \"http\" hook on PreToolUse (only command hooks run)",
            ]
        );
        let parsed = parse(
            json!({ "PreToolUse": [{ "hooks": [
                { "type": "prompt" },
                { "type": "command", "command": "sync.sh" },
                { "type": "command", "command": "bg.sh", "async": true },
            ] }] }),
            CODEX,
        );
        assert_eq!(
            parsed.groups.of(HookEvent::PreToolUse),
            [group(None, vec![hook("sync.sh", None)])]
        );
        assert_eq!(
            parsed.skipped,
            [
                "skipping unsupported \"prompt\" hook on PreToolUse (only sync command hooks run)",
                "skipping async hook on PreToolUse (only sync command hooks run)",
            ]
        );
    }

    #[test]
    fn a_hook_without_a_type_is_a_command() {
        for dialect in [CLAUDE, CODEX] {
            let parsed = parse(
                json!({ "Stop": [{ "hooks": [{ "command": "d.sh" }] }] }),
                dialect,
            );
            assert_eq!(
                parsed.groups.of(HookEvent::Stop),
                [group(None, vec![hook("d.sh", None)])]
            );
        }
    }

    #[test]
    fn malformed_entries_are_dropped() {
        for dialect in [CLAUDE, CODEX] {
            for raw in [
                json!({ "PreToolUse": "nope" }),
                json!({ "PreToolUse": [42, { "hooks": "no" }, { "hooks": [7, { "type": "command" }] }] }),
                json!({ "Stop": [{ "hooks": [{ "type": "command", "command": 5 }] }] }),
                Value::Null,
                json!(42),
                json!([1, 2]),
            ] {
                assert!(parse(raw.clone(), dialect).groups.is_empty(), "{raw}");
            }
            let parsed = parse(
                json!({ "Stop": [{ "hooks": [null, 7, { "type": "command", "command": "s.sh" }] }] }),
                dialect,
            );
            assert_eq!(
                parsed.groups.of(HookEvent::Stop),
                [group(None, vec![hook("s.sh", None)])]
            );
        }
    }

    #[test]
    fn an_invalid_matcher_rejects_the_file_with_its_event() {
        let raw = json!({ "PreToolUse": [{ "matcher": "(", "hooks": [{ "type": "command", "command": "x.sh" }] }] });
        assert_eq!(
            parse_hooks(&raw, CLAUDE).expect_err("invalid"),
            r#"invalid claude-code regex matcher "(" on event "PreToolUse""#
        );
        let raw = json!({ "PreToolUse": [{ "matcher": "[", "hooks": [{ "type": "command", "command": "s.sh" }] }] });
        assert_eq!(
            parse_hooks(&raw, CODEX).expect_err("invalid"),
            r#"invalid codex regex matcher "[" on event "PreToolUse""#
        );
    }

    #[test]
    fn matchers_of_events_without_a_subject_are_discarded() {
        for dialect in [CLAUDE, CODEX] {
            let parsed = parse(
                json!({
                    "UserPromptSubmit": [{ "matcher": "[", "hooks": [{ "type": "command", "command": "prompt.sh" }] }],
                    "Stop": [{ "matcher": "(", "hooks": [{ "type": "command", "command": "stop.sh" }] }],
                }),
                dialect,
            );
            assert_eq!(
                parsed.groups.of(HookEvent::UserPromptSubmit),
                [group(None, vec![hook("prompt.sh", None)])]
            );
            assert_eq!(
                parsed.groups.of(HookEvent::Stop),
                [group(None, vec![hook("stop.sh", None)])]
            );
        }
    }

    #[test]
    fn unsupported_events_are_ignored_before_validation() {
        let parsed = parse(
            json!({
                "Setup": [{ "matcher": "(", "hooks": [{ "type": "command", "command": "ignored.sh" }] }],
                "Notification": [{ "hooks": [{ "type": "command", "command": "c.sh" }] }],
                "PreToolUse": [{ "matcher": "Bash", "hooks": [{ "type": "command", "command": "kept.sh" }] }],
            }),
            CLAUDE,
        );
        assert_eq!(
            parsed.groups.of(HookEvent::PreToolUse),
            [group(Some("Bash"), vec![hook("kept.sh", None)])]
        );
        assert!(parsed.groups.of(HookEvent::Stop).is_empty());
        assert!(parsed.skipped.is_empty());
    }

    #[test]
    fn claude_subagent_events_are_reported_as_not_run() {
        let parsed = parse(
            json!({
                "SubagentStart": [{ "hooks": [{ "command": "a.sh" }] }],
                "SubagentStop": [{ "hooks": [{ "command": "b.sh" }] }],
            }),
            CLAUDE,
        );
        assert!(parsed.groups.is_empty());
        assert_eq!(
            parsed.skipped,
            [
                "SubagentStart hooks are not run by bake",
                "SubagentStop hooks are not run by bake",
            ]
        );
        let parsed = parse(
            json!({ "SubagentStop": [{ "hooks": [{ "command": "b.sh" }] }] }),
            CODEX,
        );
        assert!(parsed.groups.is_empty() && parsed.skipped.is_empty());
    }
}
