/**
 * Integration suites (`*.e2e.ts`): the assembled CLI profiles, real sandbox
 * backends, built artifacts, and real-provider smokes. Provider keys are
 * removed unless `DSH_E2E_LIVE=1`, so a default run spends no tokens and every
 * key-gated test skips itself.
 */
import { runtimeSuite } from './vitest.shared.ts'

export default runtimeSuite({
  include: ['packages/*/*/tests/**/*.e2e.ts', 'apps/cli/tests/**/*.e2e.ts'],
  setupFiles: ['./scripts/test-live-keys.ts'],
  // Real processes, packs, and model turns take longer than a unit spec.
  testTimeout: 120_000,
  hookTimeout: 60_000,
})
