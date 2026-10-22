// The download service's routes, shared by the Cloudflare Worker that serves
// https://bake.justar.dev (worker.mjs) and the Node server the release checks
// run locally (server.mjs). Each runtime supplies how a served file is read
// and who asked; the routes, headers, redirects, and activity reports are
// decided here alone.

// Archives this service does not hold are served by the GitHub release of
// their version. Clients check every archive against the signed manifest, so
// the redirect grants that host no trust.
const archivePath = /^\/releases\/([0-9A-Za-z.-]+)\/(bake-v[0-9A-Za-z.-]+-(?:darwin-(?:arm64|x64)|linux-(?:arm64|x64)|win32-x64)\.tar\.gz)$/
// The native route: its own manifest, signature, and archives, which no 0.3
// updater reads. Its archives redirect to the same GitHub release tag.
const nativeArchivePath = /^\/native\/releases\/([0-9A-Za-z.-]+)\/(bake-v[0-9A-Za-z.-]+-(?:darwin-(?:arm64|x64)|linux-(?:arm64|x64)|win32-x64)\.tar\.gz)$/
const MAX_REPORTED_PER_DAY = 100_000

/**
 * @typedef {object} Served
 * @property {BodyInit | null} body - the file's bytes.
 * @property {number} size - the file's length in bytes.
 */

/**
 * @typedef {object} HandlerOptions
 * @property {(path: string) => Promise<Served | undefined>} read - a served file by its path
 *   under the site root (`index.html`, `install.sh`, `latest.json`, `native/stable.json`,
 *   `releases/<version>/<archive>`), or undefined when the service does not hold it.
 * @property {string} [archiveRedirect] - the GitHub releases download URL archives redirect to.
 * @property {string} [activityUrl] - gissx.org's activity endpoint.
 * @property {string} [activityToken] - its bearer token.
 * @property {string} [activitySalt] - the salt of each day's address hash.
 */

/**
 * @typedef {object} RequestContext
 * @property {string} [address] - the client's address, which only its daily hash leaves this service with.
 * @property {(work: Promise<void>) => void} [waitUntil] - keeps an activity report alive past the response.
 */

/**
 * Build the service's request handler.
 * @param {HandlerOptions} options - how files are read, and where archives and reports go.
 * @returns {(request: Request, context?: RequestContext) => Promise<Response>} the handler.
 */
export function createHandler(options) {
  const archiveRedirect = options.archiveRedirect?.trim().replace(/\/+$/, '') || undefined
  // Active installs, reported to gissx.org when all three are set. An installed
  // Bake fetches its channel's manifest at most hourly to check for updates,
  // unless BAKE_NO_UPDATE_CHECK is set. The TypeScript agent uses Node's own
  // client, so each `node` request for latest.json is a 0.3 install in use,
  // while installers, browsers, and release checks use other clients. The
  // native channel is counted by its route instead, so the native updater needs
  // no user-agent contract: every request for native/stable.json counts,
  // including an install that is reading the manifest to install. The two
  // channels are counted apart, the native one as channel `native` of the same
  // product, and a day's count holds one entry per address and channel. The
  // install is sent as a daily salted hash of its address, at most once a day
  // per process or Worker isolate, so no address leaves this service.
  // Reporting never delays or fails the response.
  const activityUrl = options.activityUrl?.trim() || undefined
  const activityToken = options.activityToken?.trim() || undefined
  const activitySalt = options.activitySalt?.trim() || undefined
  const reportedToday = { day: '', visitors: new Set() }

  /**
   * Report one install's update check, unless this process already did today.
   * @param {Request} request - the manifest request.
   * @param {string} address - its client's address.
   * @param {string} [channel] - `native`, or undefined for the 0.3 channel.
   * @returns {Promise<void>} settles when the report is sent or dropped; never rejects.
   */
  async function reportActive(request, address, channel) {
    if (activityUrl === undefined || activityToken === undefined || activitySalt === undefined) return
    if (channel === undefined && !/^node(\/|$)/i.test(request.headers.get('user-agent') ?? '')) return
    const day = new Date().toISOString().slice(0, 10)
    // The 0.3 hash input is unchanged, so its running count carries on; a channel
    // of its own hashes apart, and counts the same address once more.
    const named = [activitySalt, day, address, 'bake', ...channel === undefined ? [] : [channel]]
    const digest = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(named.join('\n')))
    const visitor = Array.from(new Uint8Array(digest).slice(0, 8), byte => byte.toString(16).padStart(2, '0')).join('')
    // Checked and recorded in one turn, so two concurrent checks report once.
    if (reportedToday.day !== day) {
      reportedToday.day = day
      reportedToday.visitors.clear()
    }
    if (reportedToday.visitors.has(visitor)) return
    if (reportedToday.visitors.size < MAX_REPORTED_PER_DAY) reportedToday.visitors.add(visitor)
    try {
      const response = await fetch(activityUrl, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${activityToken}` },
        body: JSON.stringify(channel === undefined ? { product: 'bake', visitor, day } : { product: 'bake', channel, visitor, day }),
        signal: AbortSignal.timeout(5000),
      })
      // Not counted: let a later check try again.
      if (!response.ok) reportedToday.visitors.delete(visitor)
    } catch {
      reportedToday.visitors.delete(visitor)
    }
  }

  return async function handle(request, context = {}) {
    const method = request.method
    if (method !== 'GET' && method !== 'HEAD') return new Response(null, { status: 405, headers: { Allow: 'GET, HEAD' } })
    // URL parsing resolves `.` and `..` segments, so no path below escapes the site.
    const pathname = new URL(request.url).pathname
    if (pathname === '/health') {
      return new Response(method === 'HEAD' ? null : 'ok\n', { status: 200, headers: { 'Content-Type': 'text/plain', 'Cache-Control': 'no-store' } })
    }
    const path = pathname === '/' ? 'index.html'
      : pathname === '/install.sh' || pathname === '/install.ps1' || pathname === '/latest.json' || pathname === '/latest.json.sig'
        || pathname === '/native/stable.json' || pathname === '/native/stable.json.sig'
        || archivePath.test(pathname) || nativeArchivePath.test(pathname)
        ? pathname.slice(1)
        : undefined
    if (path === undefined) return new Response(null, { status: 404 })
    const served = await options.read(path)
    if (served === undefined) {
      const archive = archiveRedirect === undefined ? null : (archivePath.exec(pathname) ?? nativeArchivePath.exec(pathname))
      if (archive !== null) {
        return new Response(null, { status: 302, headers: { Location: `${archiveRedirect}/v${archive[1]}/${archive[2]}`, 'Cache-Control': 'public, max-age=3600' } })
      }
      return new Response(null, { status: 404 })
    }
    const headers = {
      'Content-Type': path.endsWith('.html') ? 'text/html; charset=utf-8' : path.endsWith('.json') ? 'application/json; charset=utf-8' : path.endsWith('.gz') ? 'application/gzip' : 'text/plain; charset=utf-8',
      'Content-Length': String(served.size),
      'Cache-Control': path.endsWith('.gz') ? 'public, max-age=31536000, immutable' : 'no-store',
      'X-Content-Type-Options': 'nosniff',
    }
    if (method === 'HEAD') return new Response(null, { status: 200, headers })
    const waitUntil = context.waitUntil ?? (() => {})
    if (pathname === '/latest.json') waitUntil(reportActive(request, context.address ?? ''))
    else if (pathname === '/native/stable.json') waitUntil(reportActive(request, context.address ?? '', 'native'))
    return new Response(served.body, { status: 200, headers })
  }
}
