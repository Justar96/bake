# Plugins

A plugin is a WebAssembly component that can veto tool calls: before each call to a tool its manifest lists, bake asks it whether the call may run. A plugin can only block a call; one it lets through still goes through your permission preset and approvals (`bake docs permissions`). Plugins are off until you enable them in the Bake home, and bake never loads a plugin from a project directory.

## Packages

A package is a directory with two files:

- `plugin.json`, its manifest: `{"api": "0.1.0", "id": "guard", "version": "0.1.0", "vetoTools": ["edit", "write"]}`. `id` is 1 to 32 characters of `a-z`, `0-9`, and `-`; `vetoTools` lists 1 to 32 tool names.
- `plugin.wasm`, a WASI 0.2 component built for `wasm32-wasip2` against the `bake:plugin@0.1.0` API.

Each plugin runs in its own helper process, a copy of bake with no environment, no terminal, and a memory limit. The component itself can reach no file, network, environment variable, or process: its only effect is its answer and its log lines, which bake keeps out of the model's context.

## Managing plugins

| Command | What it does |
|---|---|
| `bake plugin check <dir> [<tool> <args-json>]` | Starts the package in a helper and optionally asks it about one call (`bake docs cli`) |
| `bake plugin install <dir>` | Reads `plugin.json` and `plugin.wasm` once (a symbolic link is refused), checks and compiles them in a helper, then copies those bytes into the store under their SHA-256 hash and prints it. It runs nothing from the directory, writes nothing there, and enables nothing |
| `bake plugin enable <sha256>` | Enables an installed package: records its id, hash, version, and the tools it vetoes in `plugins.json` |
| `bake plugin disable <id>` | Disables a plugin and clears a recorded failure; the package stays installed |
| `bake plugin list` | Lists the enabled plugins: `ready`, `failed`, or `unloadable` |

A session reads `plugins.json` once, when it starts, so `enable` and `disable` reach new sessions, including ones `/new` and `/resume` open, and not the running one. Only one package per plugin id can be enabled at a time: disable the old one first.

These commands exit 0 on success and 1 with an `Error:` line otherwise; `install` exits 130 after Ctrl+C, which stops its check.

## In a session

Plugins are asked in id order; the first block wins, and its reason is the call's error result, which the model sees. Every block is recorded in the session file as a `bake.plugin.decision` entry, before the result.

A plugin that fails (it traps, exceeds a limit, answers with an error, misses its deadline, breaks the helper protocol, or is refused when its helper loads it) blocks that call, and its helper is stopped. The failure is recorded in `plugins.json`, and from then on every call to the tools it lists is blocked with `Plugin "<id>" failed earlier, blocking execution: …`, in this session and every later one, until you run `bake plugin disable <id>` (and `enable` again to retry it). If the failure cannot be recorded, the run stops with that error.

A plugin whose store copy is missing, has changed, no longer matches its record (including its id), or can be written by other users blocks its tools in that session, with a startup warning, without being started. So does a plugin that cannot be started because bake's helper or its memory limit is unavailable; those two are not recorded, and the next session tries again. A `plugins.json` that is not valid stops bake at startup: repair the file, or remove it to disable every plugin.

Stopping a turn (Esc) while a plugin is checking a call stops its helper and fails nothing; the next call starts a new one. A call whose arguments are larger than 1 MiB or nested deeper than 64 levels cannot be sent to a plugin, so it is blocked without failing the plugin.

## Who can change plugins

bake trusts `plugins.json` and the store only when nobody but you and root can change them. On Linux and macOS it opens them one directory at a time from `/`, never following a link inside the `plugins` directory, and checks each opened directory and file: it must be owned by you or root and not writable by its group or others (a directory with the sticky bit, such as `/tmp`, is allowed). A symbolic link on the way to the Bake home must be yours or root's. On Linux an ACL that grants write shows in the group bits and is refused. On macOS any ACL entry that allows writing, deleting, or changing permissions or the owner is refused, whoever it names (`ls -le` lists entries, `chmod -a#` removes one); deny entries are fine. The file contents come from the opened, checked file, so a file swapped in after the check is never read. While any plugin is enabled, the `plugins` directory must also be outside the session's working directory, where the agent's own file tools could edit it, and outside any git repository. A failed check on `plugins.json` or its place stops bake at startup with the reason; one on a store copy makes that plugin unloadable, which keeps its tools blocked.

On Windows, bake cannot yet check owners and access lists, so plugins cannot be enabled there: `plugin enable` refuses, and a `plugins.json` that enables a plugin stops startup until `plugin disable` removes each record. A home without plugins works as usual, and `plugin install` and `plugin list` still work.

These checks bound who else can change the files, not everything that can: the agent's bash and file tools under `danger-full-access`, and anything else running as you, can. Without a `plugins.json` nothing is checked and nothing is enabled, so removing the file, which needs write access to the `plugins` directory, turns every plugin off. The store's read-only files do not stop their owner from changing them; bake rehashes a copy every time it reads one, and a changed copy is unloadable.

## Where plugins live

| Path in the Bake home | Contents |
|---|---|
| `plugins/plugins.json` | The enabled plugins and their recorded failures, written through a locked, atomic replacement |
| `plugins/store/<sha256>/` | One installed package, read-only; every load checks its hash |
