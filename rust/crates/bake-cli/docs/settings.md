# Settings

bake-rs reads its settings from `settings.json` in the Bake home, a JSON object read at startup. Only interactive commands write it: Ctrl+S in `/model` and `/thinking` saves `defaultProvider`, `defaultModel`, and `defaultThinkingLevel`; `/settings` saves `steeringMode`, `followUpMode`, and `compaction.enabled`; `/login`, `/logout`, and `/refresh-models` write `cliproxyapi` (`bake-rs docs models`). A write keeps every other member. A missing file means the defaults. A file that is not a JSON object produces a warning and the defaults; a member of the wrong type is ignored. Projects have no settings file of their own.

## Members

| Member | Meaning |
|---|---|
| `defaultProvider`, `defaultModel` | The model a new session starts with when `--model` is not given (`bake-rs docs models`) |
| `defaultThinkingLevel` | The thinking level when none is given: `off`, `minimal`, `low`, `medium`, `high`, `xhigh`, or `max`; `medium` when unset |
| `enabledModels` | Model patterns or globs that scope the available models when `--models` is not passed (`bake-rs docs models`) |
| `modelThinkingLevels` | Thinking levels for particular models, keyed `provider/id`, used when a new session is given no thinking level |
| `steeringMode`, `followUpMode` | How prompts sent while a turn runs are delivered: `one-at-a-time` (the default) or `all` at the next step |
| `cliproxyapi` | The CLIProxyAPI account's route, or `null` once signed out; written by `/login` (`bake-rs docs models`) |
| `defaultTools` | The tool names a session starts with, in place of `read`, `bash`, `edit`, and `write`; `+name` and `-name` add to or remove from those four |
| `maxConcurrentReadTools` | Optional integer from 1 to 64; bounds concurrent native read tools and runs batches containing other tools sequentially. Absent preserves existing unlimited parallel batches |
| `taskDocuments` | When `true`, enables the versioned `task_document` tool for plans, todos, and user-requested goals; default `false` |
| `maxToolArgumentWhitespace` | The most whitespace a streamed tool call's arguments may end with, outside string values, before bake-rs abandons the response as a retryable error; a positive integer, 2000 when unset |
| `sessionDir` | One directory to keep every project's sessions in, in place of `sessions/` in the Bake home; continuing and listing still pick the working directory's sessions |
| `blockImages` | When `true`, images are not sent to the model |
| `images.autoResize` | Whether the `read` tool shrinks large images before sending them; `true` when unset |
| `compaction` | `enabled` (default `true`), `reserveTokens` (default 16384), `keepRecentTokens` (default 20000), and `modelOverrides` keyed `provider/id` for the two token settings |
| `backgroundCompaction` | When `true`, prepares one summary while foreground work continues; requires compaction enabled and defaults to `false` |
| `contextMasking` | When `true`, experimentally replaces older eligible tool observations with logged placeholders; default `false`, with ordinary compaction retained |
| `retry` | `enabled` (default `true`), `maxRetries` (3), `baseDelayMs` (2000), and `maxAgentDelayMs` (60000) for retrying transient model errors |
| `permission` | `presets` and `defaultPreset` (`bake-rs docs permissions`) |

An invalid compaction token value is reported, by name, as an error when compaction runs. A malformed `permission` section stops the run, since its presets bound what tools may do.

`maxConcurrentReadTools` bounds batches made entirely of trusted native `read`, `ls`, `find`, and `grep` tools. Preparation and permission hooks run in assistant order; completion events follow completion order and recorded results retain assistant order. A batch containing any other tool runs sequentially, including a custom or MCP tool advertising itself as read-only. Invalid explicit values stop session creation. With the setting absent, existing unlimited parallel batches and mutation barriers apply. Cancellation waits for started work and skips queued execution; session shutdown also drains blocking native reads and listings.

With `taskDocuments: true`, the default tool set also includes `task_document`. An explicit `defaultTools` list or CLI tool selection still controls whether it is active. The tool lists, reads, and revises documents using their exact current revision reference. Documents belong to the selected session branch and remain available after compaction or resume. `claimed_complete` records the agent’s claim; it does not assert that files passed a check. The Rust SDK can record separate evidence tied to a trusted native check and declared file versions. No setting automatically creates a goal or runs checks. With `--no-session`, documents exist only for that process.

With `backgroundCompaction: true`, Bake starts a summary before the ordinary compaction threshold when the model window is greater than twice `compaction.reserveTokens`. It installs a completed summary between turns only if the summarized context still matches. New conversation messages remain; context edits, navigation, another compaction, or changed model settings discard the candidate. Ordinary compaction still runs when required. Background requests share the run’s deadline and model-call allowance, preserve one call for foreground work, and remain charged when discarded. They can add model cost without reducing waiting time. JSON mode reports their lifecycle and each attempt’s available usage; interactive mode reports committed summaries.

With `contextMasking: true`, Bake keeps the last eight complete assistant tool batches and the latest successful `read` for each recorded path and range. Older successful text output from `read`, `ls`, `find`, and `grep` may be replaced by a placeholder when its recorded details identify a trusted host read capability. Unmarked imported history and unknown tool overrides remain, as do errors, incomplete batches, explicit context edits, images, and other tools. New tool results cannot supply the reserved provenance marker themselves, including while masking is off. Recorded markers are trusted session metadata, not cryptographic authentication of imported or edited logs. Assistant calls and their result identities stay paired. Each replacement is written as a Pi context edit before the reduced context is used, so reopening reproduces it and the original result stays in the file. Turning the setting off stops new masking; it does not undo recorded edits. Ordinary foreground compaction still applies when the retained context exceeds its threshold. JSON mode reports committed masking counts and removed content bytes. This experiment can lose useful older observations or cause repeated reads; its quality and cost are not yet qualified. Compare it with `backgroundCompaction` disabled to isolate the policy.

## Example

```json
{
  "defaultProvider": "cliproxyapi",
  "defaultModel": "gpt-5.5",
  "defaultThinkingLevel": "high",
  "defaultTools": ["read", "bash", "edit", "write", "grep", "find", "ls"],
  "permission": { "defaultPreset": "workspace-write" }
}
```
