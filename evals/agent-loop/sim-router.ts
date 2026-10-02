/**
 * A simulated ing task router for routing edge-case evals. The first path
 * segment of each router URL names the behaviour, so every eval arm points
 * `subagent-model-selection.router.url` at `http://127.0.0.1:<port>/<case>`
 * and the router's own `/v1/bake/select` path follows it. Each request is
 * logged as one JSON line without the task text: the case, whether a bearer
 * token came, the task's length, the allowed routes, and the priority.
 *
 * bun evals/agent-loop/sim-router.ts [--port 18917] [--log <file>] [--token <token>]
 *
 * Cases:
 *   routed     picks the first allowed route at effort low, with a normal assessment
 *   nearest    picks it at effort max, which the route may not list, with a cautious assessment
 *   policy     picks a route outside the allowlist
 *   fallback   picks the first allowed route but marks the answer as a fallback
 *   slow       answers routed after --delay ms, past an arm's timeoutMs
 *   refusal    answers HTTP 503 with a detail
 *   malformed  answers 200 with a body that is not JSON
 *   token      any case refuses a request without the expected bearer token with HTTP 401
 */
import { appendFileSync } from 'node:fs'

const args = new Map<string, string>()
for (let index = 2; index < process.argv.length; index += 2) args.set(process.argv[index]!.replace(/^--/, ''), process.argv[index + 1] ?? '')
const port = Number(args.get('port') ?? 18917)
const log = args.get('log')
const token = args.get('token') ?? 'sim-eval-token'
const delayMs = Number(args.get('delay') ?? 3000)

interface Select {
  task?: string
  allowed_models?: { provider: string; model: string; info?: { efforts?: string[] } }[]
  priority?: string
}

const json = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } })

const server = Bun.serve({
  port, hostname: '127.0.0.1', idleTimeout: 60,
  async fetch(request) {
    const url = new URL(request.url)
    const [behaviour = '', ...rest] = url.pathname.split('/').filter(Boolean)
    const path = `/${rest.join('/')}`
    const body = request.method === 'POST' ? await request.json().catch(() => ({})) as Select : {}
    const authorized = request.headers.get('authorization') === `Bearer ${token}`
    const first = body.allowed_models?.[0]
    if (log !== undefined) {
      appendFileSync(log, JSON.stringify({
        at: new Date().toISOString(), behaviour, path, authorized, taskChars: body.task?.length ?? null,
        allowed: body.allowed_models?.map(route => ({ route: `${route.provider}/${route.model}`, efforts: route.info?.efforts ?? null })) ?? null,
        priority: body.priority ?? null,
      }) + '\n')
    }
    if (!authorized) return json({ detail: 'missing or invalid token' }, 401)
    if (path !== '/v1/bake/select' || first === undefined) return json({ detail: `no such endpoint ${path}` }, 404)
    const routed = (effort: string, status: string) => ({
      provider: first.provider, model: first.model, reasoning_effort: effort,
      reason: `simulated ${behaviour}: a two-file read needs little reasoning`,
      routing: { policy: 'sim', status, difficulty: 0.1, reasons: [`simulated ${status} assessment`] },
    })
    switch (behaviour) {
      case 'routed': return json(routed('low', 'normal'))
      case 'nearest': return json(routed('max', 'cautious'))
      case 'policy': return json({ ...routed('low', 'normal'), provider: 'sim-provider', model: 'not-allowed' })
      case 'fallback': return json({ ...routed('high', 'fallback'), fallback: true, reason: 'simulated fallback: no model is ranked' })
      case 'slow': await Bun.sleep(delayMs); return json(routed('low', 'normal'))
      case 'refusal': return json({ detail: 'simulated overload' }, 503)
      case 'malformed': return new Response('<html>not json</html>', { status: 200, headers: { 'content-type': 'text/html' } })
      default: return json({ detail: `unknown simulated case ${behaviour}` }, 404)
    }
  },
})
console.log(JSON.stringify({ simRouter: server.url.origin, log: log ?? null }))
