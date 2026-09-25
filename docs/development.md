# Development guide

English | [中文](development.zh.md)

The setup tutorial takes a new contributor from prerequisites to a checked checkout. The contributor reference that follows covers repository layout, daily workflow, and CI organization. Design rationale and implementation details belong to the linked Agent Notes and scripts.

## Setup tutorial

### Prerequisites

- Node.js supports 22.19+ and 24+. CI covers 22.19, 24, and 26; see the [Node engine floor Agent Note](../.agents/notes/implemented/process/2026-07-06-node-engine-floor.md).
- Corepack-enabled pnpm. The repo pins `pnpm@11.7.0` in `package.json`; run `corepack enable` if `pnpm --version` does not resolve through Corepack.
- Git 2.26 or newer; hook setup enables Git's worktree-specific configuration extension.
- Optional: a DeepSeek API key for the Web, headless, and ACP automation demos and real-API e2e tests.

### Windows and WSL 2

On Windows, you can develop with native tools or use WSL 2 for a Linux environment. WSL 2 is useful for verifying Linux behavior and for using Linux toolchains when native dependency compilation or filesystem permissions obstruct Windows development. Each environment needs its own runtime, build tools, and permissions; WSL is optional.

Keep the checkout, installed dependencies, and toolchain in the same operating system environment. For WSL 2, store the checkout in the Linux filesystem; for native Windows tools, use the Windows filesystem. Accessing files across the two filesystems adds overhead to I/O-intensive operations such as Git, dependency installation, and builds. See Microsoft's [file storage and performance guidance](https://learn.microsoft.com/en-us/windows/wsl/filesystems#file-storage-and-performance-across-file-systems).

Install dependencies separately in each environment because native binaries and links can differ between operating systems. Test results apply to the environment where the tests ran; Windows-specific behavior still needs native Windows validation.

### First-time setup

Install dependencies from the repo root:

```sh
pnpm install
```

The install also configures worktree-local Lefthook hooks and the `dsh-translation-pairing` Git merge driver through `scripts/install-lefthook.mjs`. The [worktree-local hooks Agent Note](../.agents/notes/implemented/process/2026-07-27-worktree-local-lefthook.md) owns the hook-path safety contract; the [automatic pairing merges Agent Note](../.agents/notes/implemented/process/2026-08-08-automatic-translation-pairing-merges.md) owns the merge driver.

If either integration is missing because dependencies were restored from cache or `postinstall` was skipped, install them manually:

```sh
node scripts/install-lefthook.mjs
```

If the wrapper rejects existing Git configuration or reports a stale lock, follow its diagnostic and the linked Agent Note rather than editing worktree metadata speculatively. After moving a checkout, rerun the wrapper to regenerate the owned path.

Run typecheck once after a fresh clone:

```sh
pnpm run typecheck
```

Setup is complete when `pnpm run typecheck` exits successfully.

## Contributor reference

### TypeScript project layout

The shared Node runtime uses `tsconfig.host.json`. Check TUI types with `bun apps/tui/scripts/tui.ts check types`. Package `tsconfig.json` files reference workspace dependencies; the root `tsconfig.json` is the editor and Project Reference entry.

`bun run verify-tsconfig-paths` checks the generated source-path map. When adding or removing a package, check its references, aggregate configuration, and workspace list together.

### Environment variables

The real DeepSeek adapter and key-backed agent demos read credentials from the environment or from a gitignored `.env` at the repo root:

```sh
DEEPSEEK_API_KEY=sk-...
DEEPSEEK_BASE_URL=https://... # optional
```

`DEEPSEEK_BASE_URL` is optional and defaults to the public API. Never commit real credentials. The real-API e2e suites self-skip when `DEEPSEEK_API_KEY` is not set.

### Git integrations

The pairing merge driver derives a conflicted `.i18n.yaml` record from the confirmed ancestor, current, and other owner blobs when both language files use Git's default text strategy and merge cleanly. It fails closed on owner conflicts, non-text merge configuration, or invalid records; after an already-stopped merge, run `pnpm run resolve-translation-pairing-conflicts`, which stages every safe pairing record and exits unsuccessfully if other pairing conflicts still need manual work. See the [bilingual documentation contract](i18n/README.md#the-pairing-contract) for the exact files and states the driver accepts.

The installer probes the exact Node/tsx driver entrypoint before publishing its worktree configuration. If that runtime later becomes unavailable, the Node-independent launcher writes Git's ordinary text result, leaves the sidecar unresolved, and prints the recovery path; restore dependencies and run `pnpm run resolve-translation-pairing-conflicts`, or run `git merge --abort`. If `pre-merge-commit` rejects an otherwise clean merge, Git leaves the complete result staged without a commit; repair the failure and run `git commit`, or abort. The [automatic pairing merges Agent Note](../.agents/notes/implemented/process/2026-08-08-automatic-translation-pairing-merges.md#failure-contract) owns the exact index and `MERGE_HEAD` states.

lefthook is configured in `lefthook.yml` as a fast local checkpoint:

- `pre-commit` verifies staged pairing records against the staged owner blobs, validates staged files with the project-free `.oxlintrc.staged.json` profile and applies Oxlint fixes with one bounded retry, regenerates `THIRD_PARTY_NOTICES.md` when a staged file is one of its inputs, checks the staged diff for whitespace errors, and runs the vendor manifest guard.
- `pre-merge-commit` performs the same index-backed pairing check before Git creates an automatic merge commit.
- `pre-push` runs `pnpm run typecheck`, which completes the Host lib phase, including generated Typert contracts, before the Client TypeScript check.

The vendor manifest guard checks that changes under `vendor/*/src` are staged with the matching `vendor/README.md` manifest update. See `vendor/README.md` before editing vendored code.

Apart from the scoped staged-record verification, the hooks intentionally do not run tests, snapshots, documentation checks, builds, or hygiene. Contributors run the [checks relevant to the changed behavior](../AGENTS.md#run-relevant-checks-locally) once; CI owns exhaustive coverage, built-artifact smokes, and the Node 22.19, 24, and 26 compatibility matrix.

Contributors can opt into the comprehensive local gate set with `pnpm run check:all`. The command is independent of the Git hooks and is not an agent instruction.

### CI gates

The keyless [CI workflow](../.github/workflows/ci.yml) groups independent gates into broad lanes and runs a smaller compatibility signal across supported Node versions. Artifact consumers wait for one build within their lane. Required benchmarks run separately on standard GitHub-hosted Linux; the [benchmark runner decision](../.agents/notes/implemented/testing/2026-09-06-standard-hosted-benchmark-runner.md) owns routing and the job timeout. The separate real-API workflow runs `pnpm run test:e2e` with its configured worker bound. See [scripts/run-gates.ts](../scripts/run-gates.ts) and the workflow files for the current gate and job inventory.

The credential-free dsh dependency-layout and dsh/vendor pack rehearsals use the existing Linux self-hosted pool only when `DSH_CI_FAILOVER_LINUX=selfhosted` and the event is a trusted master push or same-repository, non-fork, non-Dependabot pull request. All other cases, including manual dispatch, use `ubuntu-24.04`; manual publication stays hosted. See the [release rehearsal runner decision](../.agents/notes/implemented/process/2026-09-06-release-rehearsal-selfhosted.md) for persistent-store isolation and fallback limits.

### Daily commands

The root [contributor instructions](../AGENTS.md#commands) summarize common commands, while [`package.json`](../package.json) and [scripts/run-gates.ts](../scripts/run-gates.ts) own the current script and gate inventories. Select the smallest checks that cover the changed surface. Documentation changes use `pnpm run doc-sync`; package-public behavior changes also update the owning README or JSDoc, and built-artifact checks require `pnpm run build` first.

### Profile runs

Run the repository build separately before using these source-checkout demos:

```sh
pnpm run build
```

The one-shot Headless coding agent needs `DEEPSEEK_API_KEY` in the environment or repo-root `.env`:

```sh
pnpm dsh --profile headless "summarize this workspace"
```

The PTC mode demo runs the same headless profile with code presentation enabled:

```sh
pnpm run demo:ptc -- "summarize this workspace"
```

### TODO markers

Use one of three comment tags to flag known issues in the code, ordered by urgency:

- `FIXME` — an issue that should block a new release. A release should not ship with an open `FIXME` unless reviewers explicitly agree the change can be merged anyway.
- `TODO` — an issue that should be fixed soon, once we have the resources.
- `XXX` — an issue that we may fix someday; lowest priority, no commitment.

Pick the tag that matches the urgency so anyone scanning the code can tell a release blocker from a someday-maybe.

### Documenting types verbatim (`ts type-equiv`)

The [subsystems](subsystems/README.md) pages paste source-equivalent declarations together with their original JSDoc so a reader sees the exact type definition and source contract. To keep a paste from drifting when source changes, fence it as ` ```ts type-equiv ` (instead of ` ```ts `) and register it in `scripts/type-equiv.manifest.json` with the source file and symbol it mirrors:

```json
{ "doc": "docs/subsystems/session.md", "symbol": "SessionEvent", "source": "packages/core/session/src/types.ts" }
```

`pnpm run verify-type-equiv` (part of `doc-sync`) then extracts that symbol's declaration and attached JSDoc from source via the TypeScript parser and asserts the block matches both. For a class whose implementation bodies do not belong in the catalog, use ` ```ts public-api ` and set `"projection": "public-api"`; the checked projection retains the public fields, constructor, accessors, methods, and original class/member JSDoc while omitting bodies and private or protected members. Comparison ignores whitespace and non-JSDoc comments but requires every original JSDoc comment, including member documentation, so readers see the source contract beside the exact type definition. The gate enforces a 1:1 correspondence by document, symbol, and projection between primary blocks and manifest entries; a paired `.zh.md` block reuses its unsuffixed sibling's entry only when the whole tracked fence sequence is byte-identical and ordered identically. `doc-typecheck` applies the same derivative rule to compilable fences, while skipping both source-equivalence fence kinds from compilation and its opt-out ratio. When you change a documented declaration or its JSDoc, the gate fails until you update the paste; when you add or remove a primary block, update the manifest in the same change.
