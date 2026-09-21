# TUI dependency references

This reference records the APIs used by the TUI and their documentation sources, checked on 2026-09-22. The checkout matches `origin/master` after a read-only fetch. Workspace dependencies follow that checkout and its lockfile; vendored dependencies follow the pinned source and local modifications in [vendor/README.md](../vendor/README.md).

## External packages

| Dependency | Used version | Documentation and applied behavior |
|---|---|---|
| Ink | 7.1.1, npm latest | [Versioned API](https://github.com/vadimdemedes/ink/tree/v7.1.1#readme): `usePaste` manages bracketed paste separately from `useInput`; `Static` receives immutable rows; `cleanup` releases an instance; `waitUntilExit` reports renderer failures. |
| React | 19.3.0, npm latest | [TypeScript](https://react.dev/learn/typescript), [useRef](https://react.dev/reference/react/useRef), [useState](https://react.dev/reference/react/useState): typed props and same-read draft refs; effects and state are owned by React components. |
| `@types/react` | 19.3.0, npm latest | [React types](https://github.com/DefinitelyTyped/DefinitelyTyped/tree/master/types/react): declared by both TUI packages, including the application’s React imports. |
| Commander | 15.0.0, npm latest | [Versioned API](https://github.com/tj/commander.js/tree/v15.0.0#readme): `Command`, typed `opts`, `option`, and `error`; harness `parseCmdline` owns launch integration. |
| ink-testing-library | 4.0.0, npm latest | [API](https://github.com/vadimdemedes/ink-testing-library#readme): Node component rendering, input writes, captured frames, and cleanup. |
| TypeScript | upstream-pinned 6.0.3 | [Project references](https://www.typescriptlang.org/docs/handbook/project-references.html), [TSConfig](https://www.typescriptlang.org/tsconfig/): dedicated application/UI projects and a no-emit test project preserve strict checking. npm latest is 7.0.2; this TUI does not change the upstream compiler. |
| Vitest | upstream-pinned 4.1.8 | [Current guide](https://vitest.dev/guide/), [configuration](https://vitest.dev/config/), [assertions](https://vitest.dev/api/expect): Node forks, isolated resources, observable readiness, and recorded file expectations. npm latest is 5.0.1; the upstream test runner stays pinned. |
| Vite resolution, supplied by Vitest | upstream-pinned | [Shared options](https://vite.dev/config/shared-options#resolve-tsconfigpaths): native `resolve.tsconfigPaths` resolves the source graph; no TUI-owned `vite-tsconfig-paths` plugin is needed. |

Node is the harness runtime. [TTY documentation](https://nodejs.org/api/tty.html) defines the stream capability check; Ink owns mode changes. Bun builds Node ESM and runs pure tests only. Package updates beyond this TUI remain an upstream toolchain decision; “current documentation” does not imply an untested compiler or runner migration.

## Harness and vendored APIs

Every directly consumed workspace package has an owning reference below. Type-only dependencies remain explicit where they supply Session events, projection fields, or Cordis declarations.

| Dependency | Owning reference | TUI use |
|---|---|---|
| Cordis | [Primer](../docs/cordis-primer.md), [vendored source](../vendor/cordis/README.md) | Required injection, scoped service access, effect disposal, and waterfall delegation |
| Cordis Loader | [Loader](../vendor/loader/README.md) | Await settled composition before setup |
| Cordis Include | [Include](../vendor/include/README.md) | Real preset composition in isolated tests |
| Schemastery | [Vendored schema API](../vendor/schemastery/README.md), [upstream API](https://github.com/shigma/schemastery) | Validate optional choices and resolve defaults before `run` |
| `dsh-agent` | [Agent](../packages/core/agent/README.md) | Create/resume handles, model selection, status, steering, cancellation, stream frames |
| `dsh-agent-presets` | [Presets](../packages/preset/agent-presets/README.md) | Resolve and mount composition, record selection, read the recorded preset |
| `dsh-tool-subagent` | [Subagents](../packages/subagent/tool-subagent/README.md) | Profile patch mounts its model-selection settings prerequisite |
| `dsh-agent-default-model` | [Default model](../packages/core/agent-default-model/README.md) | Resolve the configured model route; real provider in test fixtures |
| `dsh-session` | [Session](../packages/core/session/README.md) | Branded identity, committed events, durable header and sequence order |
| `dsh-session-projection` | [Projections](../packages/session/session-projection/README.md) | Read authoritative inbox and preset; subscribe without mutating values |
| `dsh-session-query` | [Query](../packages/session-query/session-query/README.md) | Disposable consistent history observation |
| `dsh-session-persistence` | [Persistence](../packages/session/session-persistence/README.md) | Exact resume requires durable storage |
| `dsh-session-persistence-jsonl` | [JSONL](../packages/session/session-persistence-jsonl/README.md) | Isolated durable integration fixtures and PTY comparison |
| `dsh-llm` | [LLM](../packages/llm/llm/README.md) | Identified user messages and streaming `BlockAssembler` |
| `dsh-compaction` | [Compaction](../packages/compaction/compaction/README.md) | Typed compaction marker; ignore replacement tool rows |
| `dsh-commands` | [Commands](../packages/interaction/commands/README.md) | Parse and execute registered commands; preserve durable command output |
| `dsh-user-approval` | [Approval](../packages/interaction/user-approval/README.md) | Scoped single-use decisions and cancellation |
| `dsh-user-questions` | [Questions](../packages/interaction/user-questions/README.md) | Exact question ids, labels, custom answers, and complete plan detail |
| `dsh-authorization` | [Authorization](../packages/credentials/authorization/README.md) | List flows, present prompts, pass command cancellation |
| `dsh-credentials` | [Credentials](../packages/credentials/credentials/README.md) | Describe configured/writable references and store secret values |
| `dsh-cmdline` | [Command line](../packages/boot/cmdline/README.md) | Parse flags and use the launcher-owned exit callback |
| `dsh-file-reference` | [Reference service and grammar](../packages/context/file-reference/README.md) | Cancellable scoped discovery; pure token detection and canonical mention formatting |
| `dsh-file-reference-local` | [Local provider](../packages/context/file-reference-local/README.md) | Profile composition supplies workspace path search and model guidance |
| `dsh-fs` | [Filesystem](../packages/fs/fs/README.md) | Resolve the actual process workspace path |
| `dsh-util-values` | [Value utilities](../packages/util/values/README.md) | Exhaustive terminal row rendering with `assertNever` |
| `dsh-brand` | [Brands](../packages/util/brand/README.md) | Preserve opaque Session ids |
| `dsh-agent-loop` | [Agent loop](../packages/core/agent-loop/README.md) | Real loop in integration tests |
| `dsh-agent-loop-testkit` | [Loop test support](../packages/test-support/agent-loop-testkit/README.md) | Compose isolated real loop dependencies |
| `dsh-llm-replay` | [Recorded replay](../packages/test-support/llm-replay/README.md) | Restore the shared Session fixture and replay through the built profile |
| `@dsh-tui/ui` | [Presentation](packages/ui/README.md) | Pure typed terminal components and event projection |

The recorded bash scenario is shared read-only from [its owner](../snapshots/session/bash-tool-turn/snapshot.yml). TUI transcript expectations and profile-driving tests live under `tui/`; no upstream fixture generations are modified.
