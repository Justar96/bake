# Hooks

bake runs the command hooks of an existing Claude Code or Codex `hooks.json` during agent runs. A hook is a shell command that runs at a moment of the run, reads a JSON description of it on its standard input, and answers with its exit code and output: it can block a prompt or a tool call with a reason the model sees, add context the model sees in its next request, or keep the run going when it would stop. Nothing runs unless `settings.json` names a hooks file.

## Turning hooks on

Name the files in the `hooks` member of `settings.json` in the Bake home:

```json
{
  "hooks": {
    "claudeCode": "~/.claude/settings.json",
    "codex": "~/.codex/hooks.json"
  }
}
```

- `claudeCode` is a Claude Code `hooks.json`, or a Claude Code settings file whose `hooks` member holds one.
- `codex` is a Codex `hooks.json`.
- A path may start with `~/`; a relative path is read from the Bake home. Either member may be left out.

The files are read when a session starts, including the sessions `/new`, `/resume`, and `/fork` open. A file that is missing, refused, or not valid JSON is a warning, and none of its hooks run; the session still starts. A file with an invalid matcher runs none of its hooks either.

Because a hooks file runs commands and decides which tool calls run, bake reads it only under the rules it applies to `mcp.json` (`bake docs mcp`): it must be a regular file, not a link, of at most 1 MiB; on Unix it and the directories above it must belong to you or root and be writable by no one else; and below your home directory it must not be inside the session's working directory or a git work tree. On Windows an existing hooks file is refused unread for now.

## Events

| Event | When it runs | Its matcher selects | What a hook can do |
|---|---|---|---|
| `SessionStart` | before the first prompt bake sends in a session | `startup` for a new session, `resume` for one with a conversation | add context |
| `UserPromptSubmit` | before a prompt is sent, and before a steering or follow-up prompt is queued; the hook sees the prompt after `/skill:` and prompt-template expansion | nothing (every hook runs) | block the prompt, or add context after it |
| `PreToolUse` | before a tool call runs | the tool's name, such as `bash`, `read`, `edit`, `write`, or `mcp__files__search` | deny the call; a Claude Code hook can also ask you |
| `PostToolUse` | after a tool call, before its result is kept | the tool's name | replace the result with feedback, or add context |
| `Stop` | when a run is about to end | nothing | keep the run going with a reason |

Claude Code and Codex run `SessionStart` when a session opens; bake runs it as the session's first prompt starts, before that prompt's `UserPromptSubmit` hooks. If that prompt is stopped, as with Esc, before the `SessionStart` decision is written, the next prompt runs the hooks again.

Other events in a file are ignored. Claude Code's `SubagentStart` and `SubagentStop` hooks are reported as not run. A subagent (`bake docs settings`) runs only the `PreToolUse` and `PostToolUse` hooks.

A matcher that is missing, empty, or `*` selects everything. In a Claude Code file a matcher of letters, digits, `_`, and `|` lists exact names, such as `edit|write`; any other matcher, and every Codex matcher, is a regular expression that may match part of the name. Matchers see bake's tool names, which are lowercase.

## What a hook answers

- **Exit 2** blocks, with the hook's standard error as the reason.
- **Exit 0** succeeds. Standard output that is a JSON object can say more: `decision: "block"` and `reason`; `continue: false` and `stopReason`; and `hookSpecificOutput` with `hookEventName` (which must name the event running), `permissionDecision` (`allow`, `deny`, or `ask`) with `permissionDecisionReason`, and `additionalContext`. A Codex hook's plain standard output on `SessionStart` and `UserPromptSubmit` is context.
- **Any other exit**, a hook that cannot start, and a hook that times out are failures that do not block: the run goes on, and the failure is recorded.

When several hooks match, they run one after another in file order, and the strictest answer wins: a deny over an ask over an allow. Claude Code's file runs first; when its hooks block, deny, or ask, the Codex hooks for that moment do not run.

What each answer does:

- A blocked prompt is not sent. bake reports `Prompt blocked by UserPromptSubmit hook` with the reason.
- A denied tool call does not run; the model gets the reason as the call's error, or `blocked by PreToolUse hook`. A Claude Code `ask` asks you as an approval does (`bake docs permissions`); without an approval, as in print mode, the call is denied.
- A `PostToolUse` block replaces the call's result with the reason, or `blocked by PostToolUse hook`, as an error.
- A `Stop` block sends the model the reason, or `continue: blocked by Stop hook`, and the run goes on. The next `Stop` hook sees `stop_hook_active: true`, so a hook can let the run end the second time. One prompt is kept going at most 8 times in a row: a ninth block is recorded as `capped`, and the run ends with the note `Stop hooks kept this run going 8 times in a row, so it ended without continuing again.`, which the transcript shows and the model reads with the next prompt.
- `continue: false` stops as the hook's tool would: it blocks a prompt; a Claude Code `PreToolUse` hook denies the call with its `stopReason` and ends the run once the other calls of that response finish, and a Claude Code `PostToolUse` hook keeps the result, ends the run likewise, and passes its `stopReason` on as context; a Codex `PostToolUse` hook replaces the result with its reason or `stopReason`; and on `Stop` it lets the run end. A Codex `PreToolUse` hook's `continue: false` is ignored, as are `allow` and `ask` there.
- Context is shown to the model as a message: `SessionStart` context before the first prompt, prompt context right after the prompt in the same request (a steering or follow-up prompt's too), and `PostToolUse` context before the next request.

`systemMessage` and `updatedInput` are not used; the session file notes that a hook gave `updatedInput`.

## How hooks run

Each hook runs as `bash -c <command>` in the session's working directory, with bake's environment minus the variables whose names look like credentials (`KEY`, `PASSWORD`, `SECRET`, or `TOKEN`). Claude Code hooks also get `CLAUDE_PROJECT_DIR`, the working directory. A hook runs under the session's sandbox mode at the moment it starts (`bake docs permissions`): under `workspace-write` it can change files only in the working directory and the temporary directories. Where no sandbox backend enforces the mode, the hook does not run, which does not block. Under `danger-full-access` it runs unconfined.

The JSON on standard input has `session_id`, `transcript_path` (empty for Claude Code, `null` for Codex), `cwd`, `hook_event_name`, and the event's fields: `source`, `prompt`, `tool_name`, `tool_input`, `tool_use_id`, `tool_response` (the result's text), or `stop_hook_active`. Codex hooks also get `model`, `permission_mode: "default"`, and `turn_id`, and see a tool's input as `{ "command": … }`.

A hook's `timeout` is in seconds; without one it may run for 10 minutes. bake keeps the first 64,000 bytes of each output stream. Esc stops a hook the running prompt is waiting for, and ending the session stops it and waits until it and the processes it started are gone.

## The session file

Every time hooks run, bake writes a `bake.hook.decision` entry to the session file before the decision takes effect: the event, the decision, the tool call it concerns, and for each hook its dialect, matcher, `handlerId`, exit code, duration, and the start of its standard error. A `handlerId` such as `PreToolUse/0/1` names the hook by its place in its file: the event, the index of its matcher group under that event, and its index in the group, counting from 0. The command itself is not written, since it may hold a secret. Context and `Stop` reasons are kept as `bake.hook.context` messages, so a resumed session sends the model what it saw before.
