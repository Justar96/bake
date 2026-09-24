import { createReadStream, existsSync, statSync } from 'node:fs'
import { createServer } from 'node:http'
import { join, resolve, sep } from 'node:path'

const root = resolve(import.meta.dirname)
const publicRoot = join(root, 'public')
if (!existsSync(join(publicRoot, 'latest.json'))) throw new Error('Missing public/latest.json; assemble a release first')
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
    : pathname === '/latest.json' || /^\/releases\/[0-9A-Za-z.-]+\/bake-v[0-9A-Za-z.-]+-(?:darwin-(?:arm64|x64)|linux-(?:arm64|x64)|win32-x64)\.tar\.gz$/.test(pathname)
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
    response.writeHead(404).end()
    return
  }
  response.writeHead(200, {
    'Content-Type': file.endsWith('.html') ? 'text/html; charset=utf-8' : file.endsWith('.json') ? 'application/json; charset=utf-8' : file.endsWith('.gz') ? 'application/gzip' : 'text/plain; charset=utf-8',
    'Content-Length': size,
    'Cache-Control': file.endsWith('.gz') ? 'public, max-age=31536000, immutable' : 'no-store',
    'X-Content-Type-Options': 'nosniff',
  })
  if (method === 'HEAD') response.end()
  else createReadStream(file).pipe(response)
})
server.listen(port, port === 0 ? '127.0.0.1' : '0.0.0.0', () => console.log(`Bake downloads listening on ${(server.address()).port}`))
