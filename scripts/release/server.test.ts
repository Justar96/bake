/** The download service serves the archives it holds and redirects the rest to their GitHub release. */
import { afterEach, expect, test } from 'bun:test'
import { copyFileSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'

const servers: ReturnType<typeof Bun.spawn>[] = []
const roots: string[] = []
afterEach(async () => {
  const stopping = servers.splice(0)
  for (const server of stopping) server.kill()
  await Promise.all(stopping.map(server => server.exited))
  for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true })
})

async function serve(redirect?: string): Promise<{ base: string; root: string }> {
  const root = mkdtempSync(join(tmpdir(), 'bake-server-test-'))
  roots.push(root)
  copyFileSync(resolve(import.meta.dir, '../../distribution/host/server.mjs'), join(root, 'server.mjs'))
  mkdirSync(join(root, 'public/releases/0.1.1'), { recursive: true })
  writeFileSync(join(root, 'public/latest.json'), '{}')
  writeFileSync(join(root, 'public/releases/0.1.1/bake-v0.1.1-linux-x64.tar.gz'), 'held here')
  const env: Record<string, string | undefined> = { ...process.env, PORT: '0', BAKE_ARCHIVE_REDIRECT: redirect }
  const server = Bun.spawn(['node', join(root, 'server.mjs')], { env, stdout: 'pipe', stderr: 'inherit' })
  servers.push(server)
  const reader = server.stdout.getReader()
  let output = ''
  while (!output.includes('\n')) {
    const next = await reader.read()
    if (next.done) throw new Error('server exited before listening')
    output += new TextDecoder().decode(next.value)
  }
  reader.releaseLock()
  const port = /listening on (\d+)/.exec(output)?.[1]
  if (port === undefined) throw new Error(`unexpected server output: ${output}`)
  return { base: `http://127.0.0.1:${port}`, root }
}

test('redirects an archive it does not hold to its version tag', async () => {
  const { base } = await serve('https://github.com/Justar96/bake/releases/download/')
  const response = await fetch(`${base}/releases/0.1.1/bake-v0.1.1-win32-x64.tar.gz`, { redirect: 'manual' })
  expect(response.status).toBe(302)
  expect(response.headers.get('location')).toBe('https://github.com/Justar96/bake/releases/download/v0.1.1/bake-v0.1.1-win32-x64.tar.gz')
})

test('serves an archive it holds, and redirects nothing but archives', async () => {
  const { base } = await serve('https://github.com/Justar96/bake/releases/download')
  const held = await fetch(`${base}/releases/0.1.1/bake-v0.1.1-linux-x64.tar.gz`, { redirect: 'manual' })
  expect(held.status).toBe(200)
  expect(await held.text()).toBe('held here')
  expect((await fetch(`${base}/latest.json.sig`, { redirect: 'manual' })).status).toBe(404)
  expect((await fetch(`${base}/releases/0.1.1/other.tar.gz`, { redirect: 'manual' })).status).toBe(404)
})

test('answers 404 for a missing archive without a redirect host', async () => {
  const { base } = await serve()
  expect((await fetch(`${base}/releases/0.1.1/bake-v0.1.1-win32-x64.tar.gz`, { redirect: 'manual' })).status).toBe(404)
})
