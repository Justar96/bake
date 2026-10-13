# Settings

bake-rs reads its settings from `settings.json` in the Bake home, a JSON object read once at startup and never written. A missing file means the defaults. A file that is not a JSON object produces a warning and the defaults; a member of the wrong type is ignored. Projects have no settings file of their own.

## Members

| Member | Meaning |
|---|---|
| `defaultProvider`, `defaultModel` | The model a new session starts with when `--model` is not given (`bake-rs docs models`) |
| `defaultThinkingLevel` | The thinking level when none is given: `off`, `minimal`, `low`, `medium`, `high`, `xhigh`, or `max`; `medium` when unset |
| `modelThinkingLevels` | Thinking levels for particular models, keyed `provider/id`, used when a new session is given no thinking level |
| `defaultTools` | The tool names a session starts with, in place of `read`, `bash`, `edit`, and `write` |
| `sessionDir` | A directory to keep sessions in, in place of `sessions/` in the Bake home |
| `blockImages` | When `true`, images are not sent to the model |
| `images.autoResize` | Whether the `read` tool shrinks large images before sending them; `true` when unset |
| `compaction` | `enabled` (default `true`), `reserveTokens` (default 16384), `keepRecentTokens` (default 20000), and `modelOverrides` keyed `provider/id` for the two token settings |
| `retry` | `enabled` (default `true`), `maxRetries` (3), `baseDelayMs` (2000), and `maxAgentDelayMs` for retrying transient model errors |
| `permission` | `presets` and `defaultPreset` (`bake-rs docs permissions`) |

An invalid compaction token value is reported, by name, as an error when compaction runs. A malformed `permission` section stops the run, since its presets bound what tools may do.

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
