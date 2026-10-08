/**
 * Cross-runtime lease holder for `lease.cross-runtime.spec.ts`, run under
 * plain Node against the built package. It speaks the same line protocol as
 * the Rust `bake-session-lease-probe hold` command:
 *
 * - `<absolute-root> <id>`: create the Session with no `cwd`, append and flush
 *   `turn/start` seq 0 and `turn/end` seq 1, then print `{"state":"holding"}`
 *   while the write handle, and with it the kernel lock, stays open.
 * - stdin line `release`: close the handle, dispose the backend, print
 *   `{"state":"released"}`, and exit 0.
 * - stdin EOF: close, dispose, and exit 0 without output.
 * - any other stdin line, or bad arguments: report on stderr and exit 2.
 * - a failed create, append, flush, or close: report on stderr and exit 1.
 *
 * A SIGKILL is the crash case: no release code runs, so only the kernel can
 * drop the lock. No signal handler is installed.
 */

import { isAbsolute } from 'node:path'
import { Context } from '@deepseek-ai/cordis'
import { SESSION_FORMAT_VERSION } from 'bake-session'
import JsonlSessionPersistence from 'bake-session-persistence-jsonl'

/** Longest stdin line accepted before the input counts as a protocol error. */
const MAX_LINE = 64

const args = process.argv.slice(2)
if (args.length !== 2 || !isAbsolute(args[0]) || args[1] === '') {
  process.stderr.write('usage: lease-cross-runtime-holder.mjs <absolute-root> <id>\n')
  process.exit(2)
}
const [root, sessionId] = args

/** Write one line and exit once stdout has accepted it. */
function finish(line, code) {
  if (line === undefined) process.exit(code)
  process.stdout.write(`${line}\n`, () => process.exit(code))
}

const ctx = new Context()
let handle
try {
  await ctx.plugin(JsonlSessionPersistence, { root, compression: 'none' })
  handle = await ctx.sessionPersistence.create({
    version: SESSION_FORMAT_VERSION,
    id: sessionId,
    createdAt: 1000,
    isSeeded: false,
  })
  await handle.append([
    { type: 'turn/start', seq: 0, time: 1, data: { turn: 1 } },
    { type: 'turn/end', seq: 1, time: 2, data: { turn: 1, reason: { kind: 'completed' } } },
  ])
  await handle.flush()
} catch (error) {
  process.stderr.write(`holder setup failed: ${error instanceof Error ? error.stack : String(error)}\n`)
  process.exit(1)
}

let releasing = false
/** Close the handle, then the backend, and exit with the given final line. */
async function release(line) {
  if (releasing) return
  releasing = true
  try {
    await handle.close()
    await ctx.fiber.dispose()
  } catch (error) {
    process.stderr.write(`holder release failed: ${error instanceof Error ? error.stack : String(error)}\n`)
    process.exit(1)
  }
  finish(line, 0)
}

let pending = ''
process.stdin.setEncoding('utf8')
process.stdin.on('data', (chunk) => {
  if (releasing) return
  pending += chunk
  const newline = pending.indexOf('\n')
  if (newline < 0) {
    if (pending.length > MAX_LINE) {
      process.stderr.write('holder: stdin line too long\n')
      process.exit(2)
    }
    return
  }
  const command = pending.slice(0, newline)
  if (command !== 'release' || pending.length !== newline + 1) {
    process.stderr.write(`holder: unexpected stdin ${JSON.stringify(pending)}\n`)
    process.exit(2)
  }
  void release('{"state":"released"}')
})
process.stdin.on('end', () => {
  if (pending !== '' && !releasing) {
    process.stderr.write(`holder: unterminated stdin ${JSON.stringify(pending)}\n`)
    process.exit(2)
  }
  void release(undefined)
})

process.stdout.write('{"state":"holding"}\n')
