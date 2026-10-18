# Permissions and the sandbox

Each session has a permission: a sandbox mode, which bounds where the `bash`, `write`, and `edit` tools may change files, and an approval policy, which says whether the agent may ask for more. The permission is stored in the session file, so a resumed session keeps it.

## Sandbox modes

| Mode | Effect |
|---|---|
| `read-only` | Tools cannot change files |
| `workspace-write` | Tools can change files under the session's working directory, `/tmp` (on Unix), and the system temporary directory |
| `danger-full-access` | File changes are not restricted, and `bash` runs unconfined |

`read`, `grep`, `find`, and `ls` are never restricted, and neither are the web tools: `web_search` and `web_fetch` run in every mode and under either approval policy without asking, as in Bake 0.3. The sandbox modes bound file writes, not the network; `web_fetch` reaches only public addresses (`bake-rs docs cli`).

## Presets

A preset names a sandbox mode and an approval policy. The built-in presets are:

- `workspace-write`: the `workspace-write` mode with the `ask` policy. New sessions use it unless something else is configured.
- `danger-full-access`: the `danger-full-access` mode with the `never` policy.

`settings.json` can replace this table and choose the default for new sessions:

```json
{
  "permission": {
    "defaultPreset": "review",
    "presets": {
      "review": { "sandbox": "read-only", "approval": "never", "description": "Read and report only." }
    }
  }
}
```

Each preset needs `sandbox` and `approval`; `name` and `description` are optional. `custom` and `auto` are reserved names. Without `defaultPreset`, the default is the 0.3 home's saved preset, when it names a preset in the table, else the first preset that pairs the `workspace-write` mode with `ask`; a table without one needs `defaultPreset`.

## Command-line flags

- `--permission <preset>` selects a preset by name.
- `--sandbox <mode>` sets the sandbox mode: `read-only`, `workspace-write`, or `danger-full-access`.
- `--approval <policy>` sets the approval policy: `ask` or `never`.

They apply in that order, so `--permission workspace-write --sandbox read-only` gives `read-only` with `ask`. The result is stored in the session and kept when it is resumed.

## How bash is confined

Under `read-only` and `workspace-write`, every `bash` command runs inside a sandbox: bubblewrap (`bwrap` on `PATH`) or Landlock on Linux, and Seatbelt (`/usr/bin/sandbox-exec`) on macOS. The sandbox limits file writes only. Commands can still read any file the user can, use the network, and reach local services over sockets, and such a service can write files on a command's behalf. When no sandbox is available, which today includes every Windows host, confined commands are refused rather than run unconfined; use `danger-full-access` to run them.

## Approvals

When a tool call is denied by the sandbox, the model may retry it once with a request for wider access, which needs approval. Under the `never` policy every request is rejected. Under `ask`, someone must answer. In an interactive session the terminal shows the request: the tool, the call id, the reason, and the call's arguments. `y` then Enter allows that one call once, `n` rejects it, and Esc cancels it. An approval never covers a later call and never changes the session's permission. Print mode (`-p`) has no one to answer, so every request is refused there. Approvals fail closed: a request that is cancelled, or that waits when the session stops or the terminal closes, is refused, and nothing runs with wider access unless it is approved.
