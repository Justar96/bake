/** Run outside Vitest's module graph so forced GC observes only the production renderer. */
import { execFile } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import { promisify } from 'node:util'
import { expect, it } from 'vitest'

it('bounds retained text across thousands of renderer updates', async () => {
  const { stdout } = await promisify(execFile)(process.execPath, [
    '--expose-gc', '--max-old-space-size=192',
    fileURLToPath(new URL('./fixtures/ink-memory.mjs', import.meta.url)),
  ], {
    env: { ...process.env, NODE_ENV: 'production', NODE_OPTIONS: '' },
    timeout: 25_000,
    killSignal: 'SIGKILL',
  })
  const report = JSON.parse(stdout.split('\n')[0]!) as {
    samples: { frames: number; heapUsed: number }[]
    bytes: number
  }
  expect(report.samples.map(sample => sample.frames)).toEqual([0, 1000, 2000, 3000, 3001])
  expect(report.bytes).toBeGreaterThan(3000 * 100)
  const baseline = report.samples[0]!.heapUsed
  // Unpatched Ink retains about 100 MiB here. This allows GC/runtime variance
  // while rejecting both unbounded caches and a count-only 1000-entry cap.
  for (const sample of report.samples.slice(1)) expect(sample.heapUsed - baseline).toBeLessThan(16 * 1024 * 1024)
})
