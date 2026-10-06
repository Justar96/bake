/**
 * Test-only arm: the Bun runner with one deliberate fault, named by the first
 * argument. Driver tests use it as a negative control; the normal runner has
 * no fault switches.
 *
 * Usage: `bun test-fault-arm.ts <fault> [marker-path]`
 */

import { chmod, readFile, rm, symlink, writeFile } from 'node:fs/promises'
import { join } from 'node:path'
import { MAX_TEXT_BYTES } from './fixture.ts'
import { execute, readInput } from './runner.ts'

const [fault, marker = ''] = process.argv.slice(2)
const input = await readInput(process.stdin)
const observation: Record<string, unknown> & Awaited<ReturnType<typeof execute>> = { ...await execute(input, process.cwd()) }
const [firstPrompt = '', ...otherPrompts] = observation.prompts
const [firstEvent = {}, secondEvent = {}, ...otherEvents] = observation.events
const [firstPermission, ...otherPermissions] = observation.permissions
const workspacePath = (path: string): string => join(process.cwd(), ...path.split('/'))
const print = (text: string): Promise<void> =>
  new Promise((resolve, reject) => process.stdout.write(text, error => error ? reject(error) : resolve()))

switch (fault) {
  case 'prompt-byte':
    // Replace the final ASCII byte of the first prompt with another one.
    observation.prompts = [`${firstPrompt.slice(0, -1)}${firstPrompt.endsWith('a') ? 'b' : 'a'}`, ...otherPrompts]
    break
  case 'swap-events':
    observation.events = [secondEvent, firstEvent, ...otherEvents]
    break
  case 'permission-outcome':
    // Report the opposite decision while the files keep the real outcome.
    if (firstPermission === undefined) throw new Error('the fixture has no permission')
    observation.permissions = [{ ...firstPermission, decision: firstPermission.decision === 'allow' ? 'deny' : 'allow' }, ...otherPermissions]
    break
  case 'file-bytes':
    await writeFile(workspacePath(input.writes[0]?.path ?? 'unexpected.txt'), 'tampered\n')
    break
  case 'protected':
    await writeFile(workspacePath('check.txt'), 'tampered\n')
    break
  case 'protected-symlink': {
    // Same bytes, reached through a link to a copy outside the workspace.
    const outside = join(process.env.TMPDIR ?? '', 'check.txt')
    await writeFile(outside, await readFile(workspacePath('check.txt')))
    await rm(workspacePath('check.txt'))
    await symlink(outside, workspacePath('check.txt'))
    break
  }
  case 'oversize':
    await writeFile(workspacePath('large.bin'), Buffer.alloc(MAX_TEXT_BYTES + 1))
    break
  case 'unreadable':
    // Snapshot reads fail with a host error whose raw message names the absolute path.
    await chmod(workspacePath(input.writes[0]?.path ?? 'unexpected.txt'), 0)
    break
  case 'unknown-field':
    observation.extra = true
    break
  case 'env':
    await writeFile(marker, JSON.stringify({ env: process.env, cwd: process.cwd() }))
    break
  case 'garbage':
    await print('not an observation\n')
    process.exit(0)
  case 'no-newline':
    await print(JSON.stringify(observation))
    process.exit(0)
  case 'two-lines':
    await print(`${JSON.stringify(observation)}\n${JSON.stringify(observation)}\n`)
    process.exit(0)
  case 'flood': {
    // Write until the driver stops this process for exceeding its stdout bound.
    const block = 'x'.repeat(64 * 1024)
    for (;;) await print(block)
  }
  case 'hang':
    // Record the pid only once the hanging state is reached, then stay alive.
    await writeFile(marker, String(process.pid))
    setInterval(() => {}, 1 << 30)
    await new Promise(() => {})
    break
  default:
    throw new Error(`unknown fault ${fault}`)
}
await print(`${JSON.stringify(observation)}\n`)
