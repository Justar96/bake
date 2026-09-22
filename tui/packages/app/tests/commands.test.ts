/** Developer commands report invalid targets before building or launching a profile. */
import { expect, test } from 'bun:test'
import { tmpdir } from 'node:os'
import { resolve } from 'node:path'

const dispatcher = resolve(import.meta.dirname, '../../../scripts/tui.ts')

test.each([
  { args: ['help'], code: 0, message: 'run bun run build first' },
  { args: ['check', '--list'], code: 0, message: 'React instance identity' },
  { args: ['check', 'unknown-target'], code: 2, message: 'unknown target: unknown-target' },
  { args: ['check', 'all', 'unknown-target'], code: 2, message: 'must be used without other targets' },
  { args: ['check', '--list', 'unit'], code: 2, message: 'must be used without other targets' },
  { args: ['not-a-command'], code: 2, message: 'unknown command: not-a-command' },
])('dispatches $args from outside the checkout', async ({ args, code, message }) => {
  const child = Bun.spawn([process.execPath, dispatcher, ...args], {
    cwd: tmpdir(), stdin: 'ignore', stdout: 'pipe', stderr: 'pipe', timeout: 10_000,
  })
  try {
    const [exitCode, stdout, stderr] = await Promise.all([
      child.exited, new Response(child.stdout).text(), new Response(child.stderr).text(),
    ])
    expect(child.signalCode).toBeNull()
    expect(exitCode).toBe(code)
    expect(stdout + stderr).toContain(message)
  } finally {
    if (child.exitCode === null && child.signalCode === null) child.kill('SIGKILL')
    await child.exited
  }
}, 15_000)
