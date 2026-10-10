/**
 * Cross-runtime tearing writer for `resume.cross-runtime.spec.ts`, run under
 * plain Node against the built package. It leaves the torn tail a TypeScript
 * writer killed mid-append leaves:
 *
 * - `<absolute-root> <id> <none|zstd> <bytes>`: create the Session with no
 *   `cwd` under that compression, append and flush `turn/start` seq 0 and
 *   `turn/end` seq 1, then print `{"state":"committed"}` while the write
 *   handle, and with it the kernel lock, stays open.
 * - stdin line `tear`: append `turn/start` seq 2 at time 7 and `turn/end`
 *   seq 3 at time 8, and flush. The backend's append write stores only its first `bytes` bytes,
 *   and then prints `{"state":"torn","content":<base64>}`, the whole buffer
 *   the backend asked to append, and never settles, so no rollback runs and
 *   the handle keeps the lock until the process is killed.
 * - stdin EOF: exit 0 without closing the handle.
 * - any other stdin input, bad arguments, or a buffer of at most `bytes`
 *   bytes: report on stderr and exit 2.
 * - a failed create, append, or flush: report on stderr and exit 1.
 *
 * The tear replaces `FileHandle.prototype.writeFile` once the first turn is
 * committed: the backend appends a batch through one `writeFile` on a handle
 * opened for appending, so the tear stands at the operating-system write,
 * and the backend's own code runs unchanged. No signal handler is installed.
 */

import { writeSync } from 'node:fs'
import { open } from 'node:fs/promises'
import { isAbsolute } from 'node:path'
import { Context } from '@deepseek-ai/cordis'
import { SESSION_FORMAT_VERSION } from 'bake-session'
import JsonlSessionPersistence from 'bake-session-persistence-jsonl'

/** Longest stdin line accepted before the input counts as a protocol error. */
const MAX_LINE = 64

const args = process.argv.slice(2)
const bytes = Number(args[3])
if (args.length !== 4 || !isAbsolute(args[0]) || args[1] === '' || (args[2] !== 'none' && args[2] !== 'zstd')
  || !/^[1-9][0-9]*$/u.test(args[3] ?? '') || !Number.isSafeInteger(bytes)) {
  process.stderr.write('usage: resume-cross-runtime-writer.mjs <absolute-root> <id> <none|zstd> <bytes>\n')
  process.exit(2)
}
const [root, sessionId, compression] = args

const ctx = new Context()
let handle
try {
  await ctx.plugin(JsonlSessionPersistence, { root, compression })
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
  process.stderr.write(`writer setup failed: ${error instanceof Error ? error.stack : String(error)}\n`)
  process.exit(1)
}

/** Arm the tear: the next `writeFile` of any file handle stores `bytes` bytes and never settles. */
async function armTear() {
  const sample = await open(process.execPath, 'r')
  const prototype = Object.getPrototypeOf(sample)
  await sample.close()
  prototype.writeFile = function tornWriteFile(data) {
    const buffer = typeof data === 'string' ? Buffer.from(data, 'utf8') : Buffer.from(data.buffer, data.byteOffset, data.byteLength)
    if (buffer.length <= bytes) {
      process.stderr.write(`writer: the append buffer holds ${buffer.length} bytes, not more than ${bytes}\n`)
      process.exit(2)
    }
    writeSync(this.fd, buffer, 0, bytes)
    process.stdout.write(`${JSON.stringify({ state: 'torn', content: buffer.toString('base64') })}\n`)
    return new Promise(() => {})
  }
}

let pending = ''
let tearing = false
process.stdin.setEncoding('utf8')
process.stdin.on('data', (chunk) => {
  pending += chunk
  const newline = pending.indexOf('\n')
  if (newline < 0) {
    if (pending.length > MAX_LINE) {
      process.stderr.write('writer: stdin line too long\n')
      process.exit(2)
    }
    return
  }
  if (tearing || pending !== 'tear\n') {
    process.stderr.write(`writer: unexpected stdin ${JSON.stringify(pending)}\n`)
    process.exit(2)
  }
  tearing = true
  pending = ''
  void (async () => {
    try {
      await armTear()
      await handle.append([
        { type: 'turn/start', seq: 2, time: 7, data: { turn: 2 } },
        { type: 'turn/end', seq: 3, time: 8, data: { turn: 2, reason: { kind: 'completed' } } },
      ])
      await handle.flush()
    } catch (error) {
      process.stderr.write(`writer append failed: ${error instanceof Error ? error.stack : String(error)}\n`)
      process.exit(1)
    }
    process.stderr.write('writer: the append settled although its write never does\n')
    process.exit(2)
  })()
})
process.stdin.on('end', () => process.exit(0))

process.stdout.write('{"state":"committed"}\n')
