/**
 * Cross-runtime migrating writer for `resume.cross-runtime.spec.ts`, run
 * under plain Node against the built package. It write-opens a Session whose
 * newest generation is v0, v1, or v2, so the backend migrates it, and stops
 * the migration where a concurrent or killed writer stands:
 *
 * - `<absolute-root> <id> pause-publish <seq>`: pause once the source was
 *   read and migrated, when the backend creates its `session.migration.*`
 *   temporary file, before anything is published.
 * - `<absolute-root> <id> pause-unlink <seq>`: pause once the temporary file
 *   was linked into place and the directory synced, when the backend removes
 *   the temporary file. POSIX only: Windows publishes with a move.
 * - `<absolute-root> <id> tear <bytes>`: the backend's writes store only
 *   their first `bytes` bytes in all, so the migration's temporary file is
 *   left torn; the write that reaches the budget never settles.
 *
 * Paused, it prints `{"state":"paused"}` while holding the write lock; the
 * stdin line `go` resumes it, and stdin EOF exits 0 without resuming. A
 * resumed open appends `turn/start` at `seq` (time 3, turn 2), flushes,
 * closes, and prints `{"outcome":"opened"}`. A torn write prints
 * `{"state":"torn"}` and never settles, so the process holds the lock until
 * it is killed; stdin EOF exits 0. A write `open` refused as owned prints
 * `{"outcome":"owned","message":...}` and exits 3; any other refusal prints
 * `{"outcome":"refused","name":...,"code":...,"message":...}` and exits 1.
 * Bad arguments, other stdin input, or an open that succeeded without
 * reaching its pause report on stderr and exit 2. No signal handler is installed.
 *
 * The pauses replace `open` or `rm` of `node:fs/promises` and the tear
 * replaces `FileHandle.prototype.writeFile`, all before the built package is
 * imported, so the backend's own code runs unchanged between them.
 */

import { writeSync } from 'node:fs'
import { createRequire, syncBuiltinESMExports } from 'node:module'
import { basename, isAbsolute } from 'node:path'

const MAX_LINE = 64
const args = process.argv.slice(2)
const [root, sessionId, mode, count] = args
const number = Number(count)
if (args.length !== 4 || !isAbsolute(root ?? '') || sessionId === '' || !['pause-publish', 'pause-unlink', 'tear'].includes(mode ?? '')
  || !/^[0-9]+$/u.test(count ?? '') || !Number.isSafeInteger(number) || (mode === 'tear' && number === 0)) {
  process.stderr.write('usage: migration-cross-runtime-writer.mjs <absolute-root> <id> <pause-publish|pause-unlink|tear> <seq|bytes>\n')
  process.exit(2)
}

const fail = (message) => {
  process.stderr.write(`writer: ${message}\n`)
  process.exit(2)
}
const print = (value) => { process.stdout.write(`${JSON.stringify(value)}\n`) }

/** The next stdin line, or `null` at EOF. */
const nextLine = (() => {
  let pending = ''
  let ended = false
  const waiters = []
  const settle = () => {
    while (waiters.length > 0) {
      const newline = pending.indexOf('\n')
      if (newline >= 0) {
        const line = pending.slice(0, newline)
        pending = pending.slice(newline + 1)
        waiters.shift()(line)
      } else if (ended) {
        waiters.shift()(null)
      } else {
        if (pending.length > MAX_LINE) fail('stdin line too long')
        return
      }
    }
  }
  process.stdin.setEncoding('utf8')
  process.stdin.on('data', (chunk) => { pending += chunk; settle() })
  process.stdin.on('end', () => { ended = true; settle() })
  process.stdin.pause()
  return () => new Promise((resolve) => {
    waiters.push(resolve)
    process.stdin.resume()
    settle()
  })
})()

const isTemporary = path => basename(String(path)).startsWith('session.migration.')
let stopped = false

async function pause() {
  stopped = true
  print({ state: 'paused' })
  const line = await nextLine()
  if (line === null) process.exit(0)
  if (line !== 'go') fail(`unexpected stdin ${JSON.stringify(line)}`)
}

const require = createRequire(import.meta.url)
const promises = require('node:fs/promises')
if (mode === 'pause-publish') {
  const original = promises.open
  promises.open = async function pausedOpen(path, flags, fileMode) {
    if (!stopped && flags === 'wx' && isTemporary(path)) await pause()
    return original.call(this, path, flags, fileMode)
  }
} else if (mode === 'pause-unlink') {
  const original = promises.rm
  promises.rm = async function pausedRm(path, options) {
    if (!stopped && isTemporary(path)) await pause()
    return original.call(this, path, options)
  }
} else {
  const sample = await promises.open(process.execPath, 'r')
  const prototype = Object.getPrototypeOf(sample)
  await sample.close()
  const original = prototype.writeFile
  let remaining = number
  prototype.writeFile = function tornWriteFile(data, options) {
    const buffer = typeof data === 'string' ? Buffer.from(data, 'utf8') : Buffer.from(data.buffer, data.byteOffset, data.byteLength)
    if (buffer.length < remaining) {
      remaining -= buffer.length
      return original.call(this, data, options)
    }
    writeSync(this.fd, buffer, 0, remaining)
    stopped = true
    print({ state: 'torn' })
    process.stdin.on('end', () => process.exit(0))
    process.stdin.resume()
    return new Promise(() => {})
  }
}
syncBuiltinESMExports()

const { Context } = await import('@deepseek-ai/cordis')
const { default: JsonlSessionPersistence } = await import('bake-session-persistence-jsonl')

const ctx = new Context()
await ctx.plugin(JsonlSessionPersistence, { root, compression: 'none' })
let handle
try {
  handle = await ctx.sessionPersistence.open(sessionId, 'write')
} catch (error) {
  if (error instanceof Error && error.name === 'SessionAlreadyOwnedError') {
    print({ outcome: 'owned', message: error.message })
    process.exit(3)
  }
  print({ outcome: 'refused', name: error?.name, code: error?.code, message: error?.message })
  process.exit(1)
}
if (!stopped) fail('the open reached no pause')
try {
  await handle.append([{ type: 'turn/start', seq: number, time: 3, data: { turn: 2 } }])
  await handle.flush()
  await handle.close()
  await ctx.fiber.dispose()
} catch (error) {
  process.stderr.write(`writer append failed: ${error instanceof Error ? error.stack : String(error)}\n`)
  process.exit(1)
}
print({ outcome: 'opened' })
process.exit(0)
