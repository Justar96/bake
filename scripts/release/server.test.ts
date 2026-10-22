/** The download service serves the archives it holds and redirects the rest to their GitHub release. */
import { afterEach, expect, setDefaultTimeout, test } from 'bun:test'
import { copyFileSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'

// Each test starts a Node server; a cold Windows runner took over Bun's
// default 5 s to start the first one.
setDefaultTimeout(30_000)

const servers: ReturnType<typeof Bun.spawn>[] = []
const roots: string[] = []
afterEach(async () => {
  const stopping = servers.splice(0)
  for (const server of stopping) server.kill()
  await Promise.all(stopping.map(server => server.exited))
  for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true })
})

async function serve(redirect?: string, extra: Record<string, string> = {}): Promise<{ base: string; root: string }> {
  const root = mkdtempSync(join(tmpdir(), 'bake-server-test-'))
  roots.push(root)
  for (const file of ['server.mjs', 'handler.mjs']) copyFileSync(resolve(import.meta.dir, '../../distribution/host', file), join(root, file))
  mkdirSync(join(root, 'public/releases/0.1.1'), { recursive: true })
  mkdirSync(join(root, 'public/native/releases/0.4.0'), { recursive: true })
  writeFileSync(join(root, 'public/latest.json'), '{}')
  writeFileSync(join(root, 'public/releases/0.1.1/bake-v0.1.1-linux-x64.tar.gz'), 'held here')
  writeFileSync(join(root, 'public/native/stable.json'), '{"channel":"stable"}')
  writeFileSync(join(root, 'public/native/stable.json.sig'), 'signature\n')
  writeFileSync(join(root, 'public/native/releases/0.4.0/bake-v0.4.0-linux-x64.tar.gz'), 'native bytes')
  const env: Record<string, string | undefined> = { ...process.env, PORT: '0', BAKE_ARCHIVE_REDIRECT: redirect, ...extra }
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
  expect((await fetch(`${base}/native/releases/0.4.0/bake-v0.4.0-win32-x64.tar.gz`, { redirect: 'manual' })).status).toBe(404)
  // Both routes name an archive alike, and each serves only its own directory,
  // so the native archive this server holds is not on the 0.3 route.
  expect((await fetch(`${base}/releases/0.4.0/bake-v0.4.0-linux-x64.tar.gz`, { redirect: 'manual' })).status).toBe(404)
})

test('serves the native route, and redirects a native archive it does not hold', async () => {
  const { base } = await serve('https://github.com/Justar96/bake/releases/download')
  expect(await (await fetch(`${base}/native/stable.json`)).text()).toBe('{"channel":"stable"}')
  expect(await (await fetch(`${base}/native/stable.json.sig`)).text()).toBe('signature\n')
  const held = await fetch(`${base}/native/releases/0.4.0/bake-v0.4.0-linux-x64.tar.gz`, { redirect: 'manual' })
  expect(held.status).toBe(200)
  expect(await held.text()).toBe('native bytes')
  const redirected = await fetch(`${base}/native/releases/0.4.0/bake-v0.4.0-win32-x64.tar.gz`, { redirect: 'manual' })
  expect(redirected.status).toBe(302)
  expect(redirected.headers.get('location')).toBe('https://github.com/Justar96/bake/releases/download/v0.4.0/bake-v0.4.0-win32-x64.tar.gz')
  // Nothing else under native/ is served, and a 0.3 request that names this
  // archive is answered from the 0.3 release tag, never with the native bytes.
  expect((await fetch(`${base}/native/stable.json.bak`, { redirect: 'manual' })).status).toBe(404)
  const onLegacyRoute = await fetch(`${base}/releases/0.4.0/bake-v0.4.0-linux-x64.tar.gz`, { redirect: 'manual' })
  expect(onLegacyRoute.status).toBe(302)
  expect(onLegacyRoute.headers.get('location')).toBe('https://github.com/Justar96/bake/releases/download/v0.4.0/bake-v0.4.0-linux-x64.tar.gz')
})

/** A stand-in for gissx.org's activity endpoint that keeps what it is sent. */
function collector(status = 204) {
  const reports: { authorization: string | null; body: { product: string; channel?: string; visitor: string; day: string } }[] = []
  const server = Bun.serve({ port: 0, hostname: '127.0.0.1', async fetch(request) {
    reports.push({ authorization: request.headers.get('authorization'), body: await request.json() as never })
    return new Response(null, { status })
  } })
  return { url: `${server.url.origin}/api/activity`, reports, stop: () => server.stop(true) }
}

const settle = () => new Promise(resolve => setTimeout(resolve, 200))

test('reports an installed Bake checking for updates once a day, as a hash, and nothing else', async () => {
  const sink = collector()
  try {
    const { base } = await serve(undefined, { GISSX_ACTIVITY_URL: sink.url, GISSX_ACTIVITY_TOKEN: 'token', BAKE_ACTIVITY_SALT: 'salt' })
    // Bake's updater, twice: one install, one report.
    for (let i = 0; i < 2; i++) expect((await fetch(`${base}/latest.json`, { headers: { 'user-agent': 'node' } })).status).toBe(200)
    // An installer, a browser, a HEAD, and the signature are not update checks.
    await fetch(`${base}/latest.json`, { headers: { 'user-agent': 'curl/8.9.1' } })
    await fetch(`${base}/latest.json`, { headers: { 'user-agent': 'Mozilla/5.0' } })
    await fetch(`${base}/latest.json`, { method: 'HEAD', headers: { 'user-agent': 'node' } })
    await fetch(`${base}/install.sh`, { headers: { 'user-agent': 'node' } })
    await settle()
    expect(sink.reports).toHaveLength(1)
    const [report] = sink.reports
    expect(report!.authorization).toBe('Bearer token')
    expect(report!.body.product).toBe('bake')
    expect(report!.body.day).toBe(new Date().toISOString().slice(0, 10))
    expect(report!.body.visitor).toMatch(/^[0-9a-f]{16}$/)
    expect(JSON.stringify(report!.body)).not.toContain('127.0.0.1')
    // Another address is another install.
    await fetch(`${base}/latest.json`, { headers: { 'user-agent': 'node', 'x-real-ip': '203.0.113.9' } })
    await settle()
    expect(sink.reports).toHaveLength(2)
    expect(sink.reports[1]!.body.visitor).not.toBe(report!.body.visitor)
  } finally { sink.stop() }
})

test('counts a native install by its route, as the native channel, whatever client reads it', async () => {
  const sink = collector()
  try {
    const { base } = await serve(undefined, { GISSX_ACTIVITY_URL: sink.url, GISSX_ACTIVITY_TOKEN: 'token', BAKE_ACTIVITY_SALT: 'salt' })
    // Any client checking the native manifest twice is one install.
    expect((await fetch(`${base}/native/stable.json`, { headers: { 'user-agent': 'bake/0.4.0' } })).status).toBe(200)
    expect((await fetch(`${base}/native/stable.json`, { headers: { 'user-agent': 'curl/8.9.1' } })).status).toBe(200)
    // Nothing else on the route is a check.
    await fetch(`${base}/native/stable.json`, { method: 'HEAD' })
    await fetch(`${base}/native/stable.json.sig`)
    await settle()
    expect(sink.reports).toHaveLength(1)
    expect(sink.reports[0]!.body).toMatchObject({ product: 'bake', channel: 'native' })
    expect(sink.reports[0]!.body.visitor).toMatch(/^[0-9a-f]{16}$/)
    expect(JSON.stringify(sink.reports[0]!.body)).not.toContain('127.0.0.1')
    // The same address on the 0.3 channel is counted once more, under an unrelated hash.
    await fetch(`${base}/latest.json`, { headers: { 'user-agent': 'node' } })
    await settle()
    expect(sink.reports).toHaveLength(2)
    expect(sink.reports[1]!.body.product).toBe('bake')
    expect(sink.reports[1]!.body.channel).toBeUndefined()
    expect(sink.reports[1]!.body.visitor).not.toBe(sink.reports[0]!.body.visitor)
  } finally { sink.stop() }
})

test('retries an install whose report was refused, and reports nothing unless configured', async () => {
  const refusing = collector(503)
  try {
    const { base } = await serve(undefined, { GISSX_ACTIVITY_URL: refusing.url, GISSX_ACTIVITY_TOKEN: 'token', BAKE_ACTIVITY_SALT: 'salt' })
    await fetch(`${base}/latest.json`, { headers: { 'user-agent': 'node' } })
    await settle()
    await fetch(`${base}/latest.json`, { headers: { 'user-agent': 'node' } })
    await settle()
    expect(refusing.reports).toHaveLength(2)
  } finally { refusing.stop() }
  const idle = collector()
  try {
    const { base } = await serve(undefined, { GISSX_ACTIVITY_URL: idle.url, GISSX_ACTIVITY_TOKEN: 'token' })
    expect((await fetch(`${base}/latest.json`, { headers: { 'user-agent': 'node' } })).status).toBe(200)
    await settle()
    expect(idle.reports).toHaveLength(0)
  } finally { idle.stop() }
})

test('serves the manifest when the activity endpoint cannot be reached', async () => {
  const { base } = await serve(undefined, { GISSX_ACTIVITY_URL: 'http://127.0.0.1:9/api/activity', GISSX_ACTIVITY_TOKEN: 'token', BAKE_ACTIVITY_SALT: 'salt' })
  const response = await fetch(`${base}/latest.json`, { headers: { 'user-agent': 'node' } })
  expect(response.status).toBe(200)
  expect(await response.text()).toBe('{}')
})
