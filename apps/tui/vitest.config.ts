/**
 * Node test runner for the TUI's `*.spec.ts(x)` suites.
 *
 * The split is deliberate and matches AGENTS.md. `*.test.ts` files are pure and
 * run under `bun test`, while `*.spec.ts(x)` files mount real Cordis trees,
 * real agents, and Ink's real input channels, so they run on the runtime the
 * product ships to. `bun test` cannot host them — it provides no `vi.waitFor` —
 * and running them there would test a runtime we never ship.
 *
 * Standalone, not an extension of the repository config. This fork owns
 * `apps/tui/` only, and upstream's suites must keep meaning what upstream means.
 */

import { defineConfig } from 'vitest/config'

// The suite mounts React components and drives them through `act`, which React
// ships only in its development bundle; Vite picks that bundle from `NODE_ENV`,
// so a shell exporting `NODE_ENV=production` loads production React, where
// `act` is stripped and every mounted test crashes with `act is not a
// function`. Repair only that case. An unset `NODE_ENV`, which vitest defaults
// to `test`, and an explicit `development` stay untouched.
if (process.env.NODE_ENV === 'production') process.env.NODE_ENV = 'test'

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
