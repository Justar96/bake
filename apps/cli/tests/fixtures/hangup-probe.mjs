/**
 * Test-only Cordis plugin for terminal-hangup shutdown.
 *
 * It owns a managed subprocess tree whose members ignore SIGTERM and SIGHUP,
 * with the grace `DSH_TEST_HANGUP_GRACE_MS` allows before SIGKILL. Once the
 * test creates `hang-up` in `DSH_TEST_HANGUP_DIR`, the first streamed text of a
 * model response sends the agent a follow-up, as a user typing while the reply
 * streams does. The session event that records it makes the process send
 * itself SIGHUP twice, as one terminal close can. The event is appended in the
 * same task while the response is still in flight, so it is still in the
 * persistence write batch when the first signal arrives; `trigger.json` names
 * it. The disposer writes to stdout and stderr, as teardown diagnostics do.
 */

import { randomUUID } from 'node:crypto'
import { existsSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'

export const name = 'hangup-probe'
export const inject = ['subprocess']

/** The follow-up the simulated user sends while the reply streams; the test matches this text. */
const FOLLOW_UP = 'follow-up typed during the reply'

/**
 * Spawn the tree, arm the hangup, and register the writing disposer.
 * @param {import('@deepseek-ai/cordis').Context} ctx - loader-mounted test plugin context.
 */
export function apply(ctx) {
  const dir = process.env.DSH_TEST_HANGUP_DIR
  if (dir === undefined) throw new Error('hangup-probe: DSH_TEST_HANGUP_DIR is required')
  const handle = ctx.subprocess.spawn({
    argv: [process.execPath, fileURLToPath(new URL('./hangup-tree.mjs', import.meta.url)), join(dir, 'tree.json')],
    cwd: dir,
    stdio: { stdin: 'ignore', stdout: { maxBytes: 1024 }, stderr: { maxBytes: 1024 } },
    graceMs: Number(process.env.DSH_TEST_HANGUP_GRACE_MS ?? '100'),
  })
  handle.done.catch(() => {})
  let armed = false
  let hungUp = false
  ctx.on('agent/assistant-stream', ({ agent, frame }) => {
    if (armed || frame.type !== 'chunk' || frame.chunk.type !== 'text-delta') return
    if (!existsSync(join(dir, 'hang-up'))) return
    armed = true
    agent.followup({ role: 'user', id: randomUUID(), content: [{ type: 'text', text: FOLLOW_UP }], source: { kind: 'user' } })
  })
  ctx.on('session/event', (session, event) => {
    if (!armed || hungUp) return
    hungUp = true
    writeFileSync(join(dir, 'trigger.json'), JSON.stringify({ session: session.id, type: event.type, seq: event.seq }))
    process.once('SIGHUP', () => { setImmediate(() => { process.kill(process.pid, 'SIGHUP') }) })
    process.kill(process.pid, 'SIGHUP')
  })
  ctx.effect(() => () => {
    process.stdout.write('hangup-probe: disposing\n')
    process.stderr.write('hangup-probe: disposing\n')
  })
}
