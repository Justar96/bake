/** Focused Node tests for the runtime retained by Bake. TUI tests have their own runner. */
import { runtimeSuite } from './vitest.shared.ts'

export default runtimeSuite({
  // Script specs here load Cordis, the Node resolver, or built output; other script tests are `.test.ts` under `bun test`.
  include: ['packages/*/*/tests/**/*.spec.ts', 'apps/cli/tests/**/*.spec.ts', 'scripts/**/*.spec.ts'],
  exclude: ['**/*.client.spec.ts'],
  testTimeout: 30_000,
  hookTimeout: 30_000,
})
