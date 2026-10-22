/** The Cloudflare Worker serves the staged site with the Node server's routes, and redirects every archive. */
import { afterEach, expect, test } from 'bun:test'
import { createHash } from 'node:crypto'
import { resolve } from 'node:path'

const WORKER = resolve(import.meta.dir, '../../distribution/host/worker.mjs')

/** The staged site the ASSETS binding reads; it never holds an archive. */
const SITE: Record<string, string> = {
  '/index.html': '<h1>Bake</h1>',
  '/install.sh': '#!/bin/sh\n',
  '/install.ps1': "throw 'refused'\n",
  '/latest.json': '{}',
  '/latest.json.sig': 'legacy signature\n',
  '/native/stable.json': '{"channel":"stable"}',
  '/native/stable.json.sig': 'signature\n',
}

const pending: Promise<unknown>[] = []
afterEach(() => { pending.splice(0) })

type Context = { waitUntil: (work: Promise<unknown>) => void }
type Worker = { fetch: (request: Request, env: Record<string, unknown>, ctx: Context) => Promise<Response> }

let loaded = 0

/** A fresh Worker module, so each test's variables build its own handler. */
async function worker(vars: Record<string, string> = {}): Promise<(path: string, init?: RequestInit) => Promise<Response>> {
  const module = await import(`${WORKER}?test=${++loaded}`) as { default: Worker }
  const env = {
    BAKE_ARCHIVE_REDIRECT: 'https://github.com/Justar96/bake/releases/download',
    ...vars,
    ASSETS: {
      async fetch(input: URL) {
        const body = SITE[new URL(input).pathname]
        return body === undefined ? new Response('missing', { status: 404 }) : new Response(body)
      },
    },
  }
  const ctx = { waitUntil: (work: Promise<unknown>) => { pending.push(work) } }
  return (path, init) => module.default.fetch(new Request(`https://bake.justar.dev${path}`, init), env, ctx)
}

test('serves the staged files with their types, uncached, and answers health', async () => {
  const get = await worker()
  const page = await get('/')
  expect(page.status).toBe(200)
  expect(page.headers.get('content-type')).toBe('text/html; charset=utf-8')
  expect(await page.text()).toBe('<h1>Bake</h1>')
  const manifest = await get('/native/stable.json')
  expect(manifest.headers.get('content-type')).toBe('application/json; charset=utf-8')
  expect(manifest.headers.get('cache-control')).toBe('no-store')
  expect(manifest.headers.get('x-content-type-options')).toBe('nosniff')
  expect(await manifest.text()).toBe('{"channel":"stable"}')
  const installer = await get('/install.sh')
  expect(installer.headers.get('content-type')).toBe('text/plain; charset=utf-8')
  expect(installer.headers.get('content-length')).toBe('10')
  expect(await installer.text()).toBe('#!/bin/sh\n')
  expect(await (await get('/install.ps1')).text()).toBe("throw 'refused'\n")
  expect(await (await get('/latest.json.sig')).text()).toBe('legacy signature\n')
  const health = await get('/health')
  expect(health.status).toBe(200)
  expect(await health.text()).toBe('ok\n')
  const head = await get('/native/stable.json', { method: 'HEAD' })
  expect(head.status).toBe(200)
  expect(await head.text()).toBe('')
})

test('redirects each archive to its GitHub release, on both routes', async () => {
  const get = await worker()
  const native = await get('/native/releases/0.4.0/bake-v0.4.0-linux-arm64.tar.gz', { redirect: 'manual' })
  expect(native.status).toBe(302)
  expect(native.headers.get('location')).toBe('https://github.com/Justar96/bake/releases/download/v0.4.0/bake-v0.4.0-linux-arm64.tar.gz')
  const legacy = await get('/releases/0.3.8/bake-v0.3.8-linux-x64.tar.gz', { redirect: 'manual' })
  expect(legacy.status).toBe(302)
  expect(legacy.headers.get('location')).toBe('https://github.com/Justar96/bake/releases/download/v0.3.8/bake-v0.3.8-linux-x64.tar.gz')
})

test('serves nothing else, and only GET and HEAD', async () => {
  const get = await worker()
  for (const path of ['/index.html', '/native/', '/native/stable.json.bak', '/releases/0.4.0/other.tar.gz', '/stage-site.mjs', '/wrangler.jsonc', '/public/latest.json']) {
    expect((await get(path, { redirect: 'manual' })).status).toBe(404)
  }
  const post = await get('/latest.json', { method: 'POST' })
  expect(post.status).toBe(405)
  expect(post.headers.get('allow')).toBe('GET, HEAD')
})

test('reports an update check by its connecting address, kept alive past the response', async () => {
  const reports: { authorization: string | null; body: { product: string; channel?: string; visitor: string; day: string } }[] = []
  const sink = Bun.serve({ port: 0, hostname: '127.0.0.1', async fetch(request) {
    reports.push({ authorization: request.headers.get('authorization'), body: await request.json() as never })
    return new Response(null, { status: 204 })
  } })
  try {
    const get = await worker({ GISSX_ACTIVITY_URL: `${sink.url.origin}/api/activity`, GISSX_ACTIVITY_TOKEN: 'token', BAKE_ACTIVITY_SALT: 'salt' })
    const from = (address: string, userAgent: string) => ({ headers: { 'cf-connecting-ip': address, 'user-agent': userAgent } })
    expect((await get('/native/stable.json', from('203.0.113.7', 'bake/0.4.0'))).status).toBe(200)
    expect((await get('/native/stable.json', from('203.0.113.7', 'curl/8.9.1'))).status).toBe(200)
    await get('/latest.json', from('203.0.113.7', 'curl/8.9.1'))
    await get('/latest.json', from('203.0.113.7', 'node'))
    await get('/native/stable.json', from('198.51.100.4', 'bake/0.4.0'))
    await Promise.all(pending)
    expect(reports.map(report => report.body.channel ?? '0.3').sort()).toEqual(['0.3', 'native', 'native'])
    // The Node server's hash, so the running counts carry on across hosts.
    const day = new Date().toISOString().slice(0, 10)
    const expected = createHash('sha256').update(['salt', day, '198.51.100.4', 'bake', 'native'].join('\n')).digest('hex').slice(0, 16)
    expect(reports.some(report => report.body.visitor === expected && report.body.day === day)).toBe(true)
    expect(reports.every(report => report.authorization === 'Bearer token' && /^[0-9a-f]{16}$/.test(report.body.visitor))).toBe(true)
    expect(new Set(reports.map(report => report.body.visitor)).size).toBe(3)
    expect(JSON.stringify(reports)).not.toContain('203.0.113.7')
  } finally { await sink.stop(true) }
})
