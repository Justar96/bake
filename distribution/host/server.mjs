// The download service as a Node server, over this directory's files: the
// release checks and tests run it locally, while https://bake.justar.dev runs
// the same routes as a Cloudflare Worker (worker.mjs). It serves the archives
// it holds under public/, and redirects the rest like the Worker does.
import { createReadStream, existsSync, statSync } from 'node:fs'
import { createServer } from 'node:http'
import { join, resolve, sep } from 'node:path'
import { Readable } from 'node:stream'
import { createHandler } from './handler.mjs'

const root = resolve(import.meta.dirname)
const publicRoot = join(root, 'public')
if (!existsSync(join(publicRoot, 'latest.json'))) throw new Error('Missing public/latest.json; assemble a release first')

const handle = createHandler({
  archiveRedirect: process.env.BAKE_ARCHIVE_REDIRECT,
  activityUrl: process.env.GISSX_ACTIVITY_URL,
  activityToken: process.env.GISSX_ACTIVITY_TOKEN,
  activitySalt: process.env.BAKE_ACTIVITY_SALT,
  async read(path) {
    const file = path === 'index.html' || path === 'install.sh' || path === 'install.ps1' ? join(root, path) : resolve(publicRoot, path)
    if (file !== join(root, path) && !file.startsWith(`${publicRoot}${sep}`)) return undefined
    try {
      const stat = statSync(file)
      if (!stat.isFile()) return undefined
      return { body: Readable.toWeb(createReadStream(file)), size: stat.size }
    } catch {
      return undefined
    }
  },
})

/** The client's address: a proxy in front sets X-Real-IP, or appends the last X-Forwarded-For entry. */
function clientAddress(request) {
  const real = request.headers['x-real-ip']
  if (typeof real === 'string' && real.trim() !== '') return real.trim()
  const forwarded = request.headers['x-forwarded-for']
  const last = typeof forwarded === 'string' ? forwarded.split(',').at(-1)?.trim() : undefined
  return last || request.socket.remoteAddress || ''
}

const port = Number(process.env.PORT ?? '8080')
if (!Number.isInteger(port) || port < 0 || port > 65535) throw new Error('Invalid PORT')

const server = createServer(async (request, response) => {
  try {
    const userAgent = request.headers['user-agent']
    // Request refuses methods such as CONNECT; the handler answers every
    // method but GET and HEAD alike, so any other one is passed as POST.
    const method = request.method === 'GET' || request.method === 'HEAD' ? request.method : 'POST'
    const answer = await handle(new Request(new URL(request.url ?? '/', 'http://localhost'), {
      method,
      headers: userAgent === undefined ? {} : { 'user-agent': userAgent },
    }), {
      address: clientAddress(request),
      // A report outlives its response here without help; it never rejects.
      waitUntil: () => {},
    })
    response.writeHead(answer.status, Object.fromEntries(answer.headers))
    if (answer.body === null) response.end()
    else Readable.fromWeb(answer.body).pipe(response)
  } catch (error) {
    console.error(error)
    if (!response.headersSent) response.writeHead(500)
    response.end()
  }
})
server.listen(port, port === 0 ? '127.0.0.1' : '0.0.0.0', () => console.log(`Bake downloads listening on ${(server.address()).port}`))
