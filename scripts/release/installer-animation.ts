/**
 * Embedded in each installer: only local progress events cross this boundary.
 *
 * The installer appends one event a line to the file named by the first
 * argument, its fields separated by tabs:
 *
 * - `step <label> <done>`: a step starts, and the one running finished.
 * - `note <fact>...`: facts for the running step's line.
 * - `size <path> <bytes>`: the running step fills as the file at `<path>` grows to `<bytes>`.
 * - `done <summary> <line>...`: the install finished; the summary and the lines under it close it.
 * - `stop`: the install failed or was cancelled.
 *
 * The second argument is the installer's process id; once it is gone, the
 * running step is marked failed. The third names the platform for the heading.
 */
import { readFileSync, statSync } from 'node:fs'
import { sep } from 'node:path'
import { startProgress } from '../../apps/cli/src/progress.ts'

const [events, parentArg, target] = process.argv.slice(2)
if (!events) throw new Error('Missing installer progress file')
const parent = Number(parentArg)
const progress = startProgress(process.stderr, process.env, ['installer', target ?? ''])
const home = process.env['HOME'] ?? process.env['USERPROFILE']
/** A path under the home directory, from `~`. */
const tidy = (text: string): string => home !== undefined && home !== '' && (text === home || text.startsWith(`${home}${sep}`))
  ? `~${text.slice(home.length)}` : text
const megabytes = (bytes: number): string => (bytes / 1_000_000).toFixed(1)
let consumed = 0
let watched: { readonly path: string; readonly total: number } | undefined

/** Give a watched download its size note as its step ends. */
const settleWatch = (): void => {
  if (watched !== undefined) progress.note(`${megabytes(watched.total)} MB`)
  watched = undefined
}

/** Apply one event; true once the install is over. */
const apply = ([kind = '', ...fields]: readonly string[]): boolean => {
  switch (kind) {
    case 'step':
      settleWatch()
      progress.step(fields[0] ?? '', fields[1] ?? fields[0] ?? '')
      return false
    case 'note':
      progress.note(...fields.map(tidy))
      return false
    case 'size': {
      const total = Number(fields[1])
      if (fields[0] !== undefined && fields[0] !== '' && Number.isFinite(total) && total > 0) watched = { path: fields[0], total }
      return false
    }
    case 'done':
      settleWatch()
      progress.finish({ title: fields[0] ?? '', next: fields.slice(1) })
      return true
    case 'stop':
      progress.fail()
      return true
    default:
      return false
  }
}

const timer = setInterval(() => {
  try {
    process.kill(parent, 0)
    const text = readFileSync(events, 'utf8')
    // Only whole lines: the installer may be part way through writing the last.
    const end = text.lastIndexOf('\n') + 1
    const lines = text.slice(consumed, Math.max(consumed, end)).split('\n').slice(0, -1)
    consumed = Math.max(consumed, end)
    for (const line of lines) {
      if (apply(line.replace(/\r$/u, '').split('\t'))) { clearInterval(timer); return }
    }
    if (watched !== undefined) {
      let size = 0
      try { size = statSync(watched.path).size } catch { /* not created yet */ }
      progress.progress(Math.min(1, size / watched.total), `${megabytes(Math.min(size, watched.total))} / ${megabytes(watched.total)} MB`)
    }
  } catch {
    clearInterval(timer)
    progress.fail()
  }
}, 100)
for (const signal of ['SIGINT', 'SIGTERM'] as const) {
  process.once(signal, () => { clearInterval(timer); progress.fail() })
}
