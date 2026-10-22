---
name: cordis-plugin-development
description: Use when authoring, installing, configuring, or debugging persistent plugins and MCP connections in the current Bake profile.
---

# Persistent Bake plugins

Use ordinary workspace files to author a bundle, then `plugin_manager install_bundle` to install it in the current profile. Changes affect every session in that profile and survive restart. Load `editing-cordis-compositions` for agent preset changes.

## Deliver a working plugin first

1. Resolve the requested behavior and surface. In Creator mode, decide whether it belongs in a model-facing tool, a human slash command, or both. Choose a small first version and give each contribution a unique name.
2. Discover only the APIs needed for that version: `cordis_inspect_list`, then targeted `cordis_inspect_query` calls for Host Service, Event, and Tool providers. Treat returned declarations as the supported implementation path. Resolve missing declarations through inspection before the first installation; source-level diagnosis starts from a concrete installation or runtime failure.
3. The first files you write are the installable package, patch, and required Host files in one workspace directory. Check JavaScript syntax and the manifest, then install it. Do not create a separate mock implementation or preview before exercising the installed plugin.
4. Read the installation result. After `application: applied`, invoke the new model tool or terminal command and inspect its result and activation diagnostics. State any verification limitation explicitly; installation alone does not establish that the capability works.
5. Fix observed defects in the same plugin. When the requested result works, finish with its location and verification status. Do not continue speculative features or create duplicate bundles.

## Package and install

A bundle declares `dsh.bundle.patch` in `package.json`. Its YAML patch inserts plugin entries. Give the package and rows unique names; use the Loader's existing YAML syntax, including `!!js` where expressions are needed. Read an existing patch before editing it: a matching override replaces the complete config.

This minimal Host-only package needs no install scripts or build tool. Add dependencies for packages its entry imports, using versions compatible with the active profile:

```json
{
  "name": "@local/my-tools",
  "version": "1.0.0",
  "private": true,
  "type": "module",
  "exports": { ".": "./index.js" },
  "dsh": { "bundle": { "patch": "./cordis.patch.yml" } }
}
```

`index.js` exports `export function apply() {}`. A Host plugin with behavior exports either a service class as default or named `apply`, `inject`, and optional `Config`; do not mix these forms. The bundle's `cordis.patch.yml` adds the plugin:

```yaml
- insert:
    - id: my-tools
      name: '@local/my-tools'
```

Call `plugin_manager` with `action: install_bundle` and the absolute package directory as `target`. It performs package installation and bundle selection; do not reproduce those steps with shell commands. Only pass `approvedBuilds` after the user explicitly approves the reported pending build scripts.

Use `list_plugins` or `list_bundles` to obtain exact identifiers for existing installations. `set_plugin` and `set_bundle` toggle them; `remove_bundle` removes a bundle. Inspect saved-state and activation outcomes separately: `failed` requires diagnosis, `overridden` means a higher-priority layer wins, and `restart-required` means the change is not live. Installing a new bundle can activate through HMR; replacing an installed package requires restart to load a fresh JavaScript module generation.

## Model tools

Register model-facing capabilities with `ctx.tools.register(defineTool(...))`. The tool schema is sent to the model automatically. Return the value declared by `output.schema`, keep human-readable rendering in `output.render`, and honor `exec.signal` while doing work:

```js
import { defineTool } from 'bake-tools'

export const name = 'my-tools'
export const inject = ['tools']

export function apply(ctx) {
  ctx.tools.register(defineTool({
    name: 'read_status',
    description: 'Read the current status.',
    parameters: {},
    output: {
      schema: { type: 'string' },
      render: (_args, value) => [{ type: 'text', text: value }],
    },
    async execute(_args, exec) {
      exec.signal.throwIfAborted()
      return 'ready'
    },
  }))
}
```

Tool execution must be effect-owned and cancellation-aware. Keep the canonical output useful to another tool call, and throw for infrastructure failures. Inspect the Host `tools` API for registration and execution details.

## Terminal commands

Register human-facing slash commands with `ctx.commands.register()`. A command runs directly against the receiving agent without creating a model message. Use a lowercase name, a discovery description, an optional input hint, and a handler that returns `success` or `error`:

```js
export const name = 'my-commands'
export const inject = ['commands']

export function apply(ctx) {
  ctx.commands.register({
    name: 'status',
    description: 'Show the current status',
    handler: () => ({ kind: 'success', text: 'ready' }),
  })
}
```

The command is available as `/status` in Bake. Use `input: { hint: '<value>' }` for an argument, validate the handler's input, and honor `invocation.signal` for cancellable work. The registration disposer is owned by the plugin context, so unloading the plugin removes the command.

## Lifecycle and verification

Keep factories free of side effects. Register services, tools, commands, timers, listeners, and other resources inside `apply` with Cordis effects and let their disposers run when the plugin unloads. Do not retain callbacks after disposal or start work that outlives the owning profile or agent.

After installation, run the new slash command in Bake or ask the model to call the new tool. Check the returned value, session events, and activation diagnostics. If the bundle reports `restart-required`, restart the profile before testing. Report which path you exercised and any capability that could not be tested in the current session.

## Connect an MCP server

Create a configuration-only bundle: its manifest needs a unique name, version, and `dsh.bundle.patch`, but no Host entry files. Insert the already installed MCP client in its patch:

```yaml
- insert:
    - id: demo-mcp
      name: '@deepseek-ai/dsh-mcp-client'
      config:
        serverName: demo
        transport: streamable-http
        url: http://127.0.0.1:3000/mcp
        failOnStartupError: true
```

Replace the endpoint, install the bundle through `plugin_manager`, then call `mcp__demo__ping` or another discovered tool. For stdio, use `transport: stdio`, `command`, and optional `args`, `env`, and `cwd`. Ambient credentials are scrubbed; reference existing credentials with Loader `!!js` rather than copying secrets into conversation text. Repair the same bundle on failure instead of creating duplicates.
