/**
 * Node test runner for the TUI's `*.spec.ts(x)` suites.
 *
 * The split is deliberate and matches AGENTS.md: `*.test.ts` files are pure and
 * run under `bun test`, while `*.spec.ts(x)` files mount real Cordis trees,
 * real agents, and Ink's real input channels, so they run on the runtime the
 * product ships to. `bun test` cannot host them — it provides no `vi.waitFor` —
 * and running them there would test a runtime we never ship.
 *
 * Standalone rather than an extension of the repository config: this fork owns
 * `apps/tui/` only, and upstream's suites must keep meaning what upstream means.
 */

import { defineConfig } from 'vitest/config'

export default defineConfig({
  test: {
    root: import.meta.dirname,
    include: ['packages/*/tests/**/*.spec.{ts,tsx}'],
    environment: 'node',
    // Ink's console patching needs a real `console.Console`, which vitest's
    // reporter console does not provide.
    setupFiles: ['./tests/setup-ink.ts'],
    // The integration specs drive real agents and durable persistence; the
    // default 5s timeout cuts off legitimate settlement.
    testTimeout: 30_000,
    hookTimeout: 30_000,
  },
})
