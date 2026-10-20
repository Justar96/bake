import { createHash } from 'node:crypto'
import { createReadStream, existsSync, statSync } from 'node:fs'
import { createServer } from 'node:http'
import { join, resolve, sep } from 'node:path'

const root = resolve(import.meta.dirname)
const publicRoot = join(root, 'public')
if (!existsSync(join(publicRoot, 'latest.json'))) throw new Error('Missing public/latest.json; assemble a release first')
// Archives this image does not hold are served by the GitHub release of their
// version. Clients check every archive against the signed manifest, so the
// redirect grants that host no trust.
const archiveRedirect = process.env.BAKE_ARCHIVE_REDIRECT?.trim().replace(/\/+$/, '') || undefined
const archivePath = /^\/releases\/([0-9A-Za-z.-]+)\/(bake-v[0-9A-Za-z.-]+-(?:darwin-(?:arm64|x64)|linux-(?:arm64|x64)|win32-x64)\.tar\.gz)$/
// The native route: its own manifest, signature, and archives, which no 0.3
// updater reads. Its archives redirect to the same GitHub release tag.
const nativeArchivePath = /^\/native\/releases\/([0-9A-Za-z.-]+)\/(bake-rs-v[0-9A-Za-z.-]+-(?:darwin-(?:arm64|x64)|linux-(?:arm64|x64)|win32-x64)\.tar\.gz)$/
// Active installs, reported to gissx.org when all three are set. An installed
// Bake fetches its channel's manifest at most hourly to check for updates,
// unless BAKE_NO_UPDATE_CHECK is set. The TypeScript agent uses Node's own
// client, so each `node` request for latest.json is a 0.3 install in use,
// while installers, browsers, and release checks use other clients. The native
// channel is counted by its route instead, so the native updater needs no
// user-agent contract: every request for native/stable.json counts, including
// an install that is reading the manifest to install. The two channels are
// counted apart, the native one as channel `native` of the same product, and a
// day's count holds one entry per address and channel. The install is sent as
// a daily salted hash of its address, at most once a day per process, so no
// address leaves this host. Reporting never delays or fails the response.
const activityUrl = process.env.GISSX_ACTIVITY_URL?.trim() || undefined
const activityToken = process.env.GISSX_ACTIVITY_TOKEN?.trim() || undefined
const activitySalt = process.env.BAKE_ACTIVITY_SALT?.trim() || undefined
const reportedToday = { day: '', visitors: new Set() }
const MAX_REPORTED_PER_DAY = 100_000

function clientAddress(request) {
  // Railway's edge sets X-Real-IP; X-Forwarded-For's last entry is the one its proxy appended.
  const real = request.headers['x-real-ip']
  if (typeof real === 'string' && real.trim() !== '') return real.trim()
  const forwarded = request.headers['x-forwarded-for']
  const last = typeof forwarded === 'string' ? forwarded.split(',').at(-1)?.trim() : undefined
  return last || request.socket.remoteAddress || ''
}

function reportActive(request, channel) {
  if (activityUrl === undefined || activityToken === undefined || activitySalt === undefined) return
  if (channel === undefined && !/^node(\/|$)/i.test(request.headers['user-agent'] ?? '')) return
  const day = new Date().toISOString().slice(0, 10)
  if (reportedToday.day !== day) {
    reportedToday.day = day
    reportedToday.visitors.clear()
  }
  // The 0.3 hash input is unchanged, so its running count carries on; a channel
  // of its own hashes apart, and counts the same address once more.
  const named = [activitySalt, day, clientAddress(request), 'bake', ...channel === undefined ? [] : [channel]]
  const visitor = createHash('sha256').update(named.join('\n')).digest('hex').slice(0, 16)
  if (reportedToday.visitors.has(visitor)) return
  if (reportedToday.visitors.size < MAX_REPORTED_PER_DAY) reportedToday.visitors.add(visitor)
  fetch(activityUrl, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${activityToken}` },
    body: JSON.stringify(channel === undefined ? { product: 'bake', visitor, day } : { product: 'bake', channel, visitor, day }),
    signal: AbortSignal.timeout(5000),
  }).then(response => {
    // Not counted: let a later check try again.
    if (!response.ok) reportedToday.visitors.delete(visitor)
  }, () => { reportedToday.visitors.delete(visitor) })
}

const port = Number(process.env.PORT ?? '8080')
if (!Number.isInteger(port) || port < 0 || port > 65535) throw new Error('Invalid PORT')

const server = createServer((request, response) => {
  const method = request.method
  if (method !== 'GET' && method !== 'HEAD') {
    response.writeHead(405, { Allow: 'GET, HEAD' }).end()
    return
  }
  const pathname = new URL(request.url ?? '/', 'http://localhost').pathname
  if (pathname === '/health') {
    response.writeHead(200, { 'Content-Type': 'text/plain', 'Cache-Control': 'no-store' })
    response.end(method === 'HEAD' ? undefined : 'ok\n')
    return
  }
  const file = pathname === '/' || pathname === '/install.sh' || pathname === '/install.ps1'
    ? join(root, pathname === '/' ? 'index.html' : pathname.slice(1))
    : pathname === '/latest.json' || pathname === '/latest.json.sig' || archivePath.test(pathname)
      || pathname === '/native/stable.json' || pathname === '/native/stable.json.sig' || nativeArchivePath.test(pathname)
      ? resolve(publicRoot, `.${pathname}`)
      : undefined
  if (file === undefined || (file !== publicRoot && !file.startsWith(`${publicRoot}${sep}`) && !file.startsWith(`${root}${sep}`))) {
    response.writeHead(404).end()
    return
  }
  let size
  try {
    const stat = statSync(file)
    if (!stat.isFile()) throw new Error('Not a file')
    size = stat.size
  } catch {
    const archive = archiveRedirect === undefined ? null : (archivePath.exec(pathname) ?? nativeArchivePath.exec(pathname))
    if (archive !== null) {
      response.writeHead(302, { Location: `${archiveRedirect}/v${archive[1]}/${archive[2]}`, 'Cache-Control': 'public, max-age=3600' }).end()
      return
    }
    response.writeHead(404).end()
    return
  }
  response.writeHead(200, {
    'Content-Type': file.endsWith('.html') ? 'text/html; charset=utf-8' : file.endsWith('.json') ? 'application/json; charset=utf-8' : file.endsWith('.sig') ? 'text/plain; charset=utf-8' : file.endsWith('.gz') ? 'application/gzip' : 'text/plain; charset=utf-8',
    'Content-Length': size,
    'Cache-Control': file.endsWith('.gz') ? 'public, max-age=31536000, immutable' : 'no-store',
    'X-Content-Type-Options': 'nosniff',
  })
  if (method === 'HEAD') response.end()
  else {
    if (pathname === '/latest.json') reportActive(request)
    else if (pathname === '/native/stable.json') reportActive(request, 'native')
    createReadStream(file).pipe(response)
  }
})
server.listen(port, port === 0 ? '127.0.0.1' : '0.0.0.0', () => console.log(`Bake downloads listening on ${(server.address()).port}`))
