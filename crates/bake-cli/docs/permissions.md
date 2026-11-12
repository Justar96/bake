# Permissions and the sandbox

Each session has a permission: a sandbox mode, which bounds where the `bash`, `write`, and `edit` tools may change files, and an approval policy, which says whether the agent may ask for more. The permission is stored in the session file, so a resumed session keeps it.

New sessions have full access, as in Pi: the `danger-full-access` mode with the `never` policy. Tools change any file the user can, `bash` runs unconfined, and nothing asks. To keep the sandbox, set `"permission": { "defaultPreset": "workspace-write" }` in `settings.json`, pass `--permission workspace-write`, or switch with `/permission` (below).

## Sandbox modes

| Mode | Effect |
|---|---|
| `read-only` | Tools cannot change files |
| `workspace-write` | Tools can change files under the session's working directory, `/tmp` (on Unix), and the system temporary directory |
| `danger-full-access` | File changes are not restricted, and `bash` runs unconfined |

`read`, `grep`, `find`, and `ls` are never restricted, and neither are the web tools: `web_search` and `web_fetch` run in every mode and under either approval policy without asking, as in Bake 0.3. The sandbox modes bound file writes, not the network; `web_fetch` reaches only public addresses (`bake docs cli`).

## Presets

A preset names a sandbox mode and an approval policy. The built-in presets are:

- `workspace-write` ("Workspace, ask for more"): the `workspace-write` mode with the `ask` policy.
- `danger-full-access` ("Full access"): the `danger-full-access` mode with the `never` policy. New sessions use it unless something else is configured.

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

Each preset needs `sandbox` and `approval`; `name` and `description` are optional. `custom` and `auto` are reserved names. The default for new sessions is, in order:

1. `defaultPreset`, which must name a preset in the table;
2. the 0.3 home's saved preset, when it names a preset in the table;
3. the first preset that pairs `danger-full-access` with `never`;
4. the first preset that pairs `workspace-write` with `ask`, the default through Bake 0.4.5;
5. the table's first preset.

A table with no preset is an error.

## Command-line flags

- `--permission <preset>` selects a preset by name.
- `--sandbox <mode>` sets the sandbox mode: `read-only`, `workspace-write`, or `danger-full-access`.
- `--approval <policy>` sets the approval policy: `ask` or `never`.

They apply in that order, so `--permission workspace-write --sandbox read-only` gives `read-only` with `ask`. The result is stored in the session and kept when it is resumed. A Pi session that never stored a permission opens with full access.

## Switching during a session

`/permission` lists the presets with each one's sandbox mode and approval policy, marks the one in force, and describes the selected one. Enter switches this session; Ctrl+S also saves the preset as `permission.defaultPreset` for new sessions. `/permission <preset>` switches at once. The switch is stored in the session file, the next tool call runs under it, and the next prompt tells the model the new sandbox and approval policy. A call that is already running, or waiting on an approval, keeps the access it was given.

## How bash is confined

Under `read-only` and `workspace-write`, every `bash` command runs inside a sandbox: bubblewrap (`bwrap` on `PATH`) or Landlock on Linux, and Seatbelt (`/usr/bin/sandbox-exec`) on macOS. The sandbox limits file writes only. Commands can still read any file the user can, use the network, and reach local services over sockets, and such a service can write files on a command's behalf. When no sandbox is available, which today includes every Windows host, confined commands are refused rather than run unconfined; use `danger-full-access` to run them.

## Approvals

Under a confined mode with the `ask` policy, a tool call the sandbox denies may be retried once with a request for wider access and a one-sentence justification, which needs approval. Under the `never` policy no tool offers such a request, and any request is rejected. Under `ask`, someone must answer. In an interactive session the terminal shows a panel above the composer:

```
╭──────────────────────────────────────────────────────╮
│ ◆ bash wants to run outside the sandbox  call_1      │
│   $ touch ~/.cache/tool/stamp                        │
│   “The build cache lives outside the workspace.”     │
│   y  Allow once                                      │
│ ▸ a  Allow for this session  same tool, same access  │
│   f  Switch to full access   no sandbox, no prompts  │
│   n  Deny                    add a note if you like  │
│ ↑↓ choose · Enter confirms · Esc cancels             │
╰──────────────────────────────────────────────────────╯
```

It shows the tool, the access it asks for, what the call would run or change (`bash`'s command, or the path `write` or `edit` changes), the model's justification, and the call's other arguments; PgUp and PgDn scroll a long request. Choose an answer with ↑↓ or its key, then press Enter:

- **Allow once** (`y`) runs this call with the access it asked for.
- **Allow for this session** (`a`) runs this call, and later requests of the same tool for the same mode in this session run without asking. A `bash` grant for `danger-full-access` covers every later escalated `bash` command, not only this one; it does not cover `write` or `edit`, and none covers another mode. Grants apply only while the policy is `ask`. They are stored in the session file, so a resumed session keeps them when someone can answer approvals; print mode never uses them.
- **Switch to full access** (`f`) runs this call and switches the session to the first preset that pairs `danger-full-access` with `never`, as `/permission` would. It is offered only when the preset table has one.
- **Deny** (`n`) opens a note to the model. Type why, or leave it empty, and press Enter to deny; Esc goes back to the choices. The model gets the refusal with `. The user's note: <note>` after it.

Esc cancels the request, which refuses it. Keys typed before the panel appeared, a paste, and Enter with nothing chosen never answer.

Print mode (`-p`) has no one to answer, so every request is refused there. Approvals fail closed: a request that is cancelled, or that waits when the session stops or the terminal closes, is refused, and nothing runs with wider access unless it is approved. Every request and its answer, with any grant or switch it makes, are written to the session file before the call runs.
