/** Untimed fixture authoring through built Harness Session and persistence APIs. */
import { mkdir, writeFile, stat, readdir } from 'node:fs/promises'
import { join } from 'node:path'
import { Context } from '@deepseek-ai/cordis'
import Persistence from '@deepseek-ai/dsh-session-persistence-jsonl'
import { history, reply } from './history.ts'

const [root, size] = process.argv.slice(2)
if (root === undefined || size === undefined) throw new Error('seed requires private root and turn count')
const cwd = join(root, 'workspace')
await mkdir(cwd, { recursive: true })
const { header, events, dimensions } = history(Number(size), cwd)
const ctx = new Context()
let fileBytes = 0
try {
  await ctx.plugin(Persistence, { root: join(root, 'home/sessions'), compression: 'none' })
  if (dimensions.turns > 0) {
    const handle = await ctx.sessionPersistence.create(header)
    try {
      await handle.append(events)
      await handle.flush()
    } finally { await handle.close() }
  }
} finally { await ctx.fiber.dispose() }
if (dimensions.turns > 0) {
  const rootPath = join(root, 'home/sessions')
  const files = (await readdir(rootPath, { recursive: true })).filter(path => /session\.v\d+\.jsonl$/.test(path))
  if (files.length !== 1) throw new Error('Expected one persisted synthetic session')
  fileBytes = (await stat(join(rootPath, files[0]!))).size
}
const fixture = history(1, cwd)
await writeFile(join(root, 'replay.jsonl'), [JSON.stringify({ type: 'session', ...fixture.header }), ...fixture.events.map(event => JSON.stringify(event)), ''].join('\n'))
await writeFile(join(root, 'reply.json'), JSON.stringify([{ kind: 'chunks', chunks: reply() }]))
await writeFile(join(root, 'dimensions.json'), JSON.stringify({ ...dimensions, fileBytes, sessionId: header.id }))
