#!/usr/bin/env node
/**
 * Test driver: start a mock OTLP/HTTP collector, boot the telemetry Loader
 * composition, explicitly share feedback through `/feedback` on a
 * credential-bearing turn, then persist what the collector captured to
 * `./otlp-captures.json` and what the process observed to `./telemetry-run.json`
 * for the inspect step.
 *
 * The collector URL reaches the shipped telemetry row only through
 * `DSH_TELEMETRY_OTLP_URL`, unless `DSH_TELEMETRY_E2E_ENDPOINT=unset` leaves the
 * row at its default. `DSH_TELEMETRY_E2E_MODE` sets `DSH_TELEMETRY_MODE`. A
 * non-empty `DSH_TELEMETRY_DISABLED` applies the launcher's switch patch last.
 */

import { subscribe, unsubscribe } from 'node:diagnostics_channel'
import type { ClientRequest } from 'node:http'
import { writeFile } from 'node:fs/promises'
import { createServer } from 'node:http'
import { once } from 'node:events'
import { gunzipSync } from 'node:zlib'
import { resolveConfigPath, resolveTelemetryPatch } from '@deepseek-ai/dsh-app-boot'
import type {} from '@deepseek-ai/dsh-command-feedback'
import type {} from '@deepseek-ai/dsh-session-telemetry'
import { runFixtureTurn } from '@deepseek-ai/dsh-loader-smoke'
import { bootProductionProfile } from '../../../../test-support/loader-smoke/tests/fixtures/production-profile.ts'

const configPath = process.argv[2]
if (configPath === undefined) throw new Error('session-telemetry-otel driver requires a config path')

// Every Node HTTP client request this process starts, which includes the OTLP exporter's.
const outbound: string[] = []
const observeRequest = (message: unknown): void => {
  const { request } = message as { request: ClientRequest }
  const host = request.getHeader('host')
  outbound.push(`${request.method} ${request.protocol}//${typeof host === 'string' ? host : request.host}${request.path}`)
}
subscribe('http.client.request.start', observeRequest)

const captures: unknown[] = []
const server = createServer((request, response) => {
  const chunks: Buffer[] = []
  request.on('data', chunk => chunks.push(chunk as Buffer))
  request.on('end', () => {
    const body = Buffer.concat(chunks)
    const decoded = request.headers['content-encoding'] === 'gzip' ? gunzipSync(body) : body
    captures.push(JSON.parse(decoded.toString()))
    response.writeHead(200, { 'content-type': 'application/json' }).end('{}')
  })
})
server.listen(0, '127.0.0.1')
await once(server, 'listening')
const address = server.address()
if (address === null || typeof address === 'string') throw new Error('collector has no port')
const collectorUrl = `http://127.0.0.1:${address.port}/v1/logs`
if (process.env.DSH_TELEMETRY_E2E_ENDPOINT === 'unset') delete process.env.DSH_TELEMETRY_OTLP_URL
else process.env.DSH_TELEMETRY_OTLP_URL = collectorUrl
if (process.env.DSH_TELEMETRY_E2E_MODE === undefined) delete process.env.DSH_TELEMETRY_MODE
else process.env.DSH_TELEMETRY_MODE = process.env.DSH_TELEMETRY_E2E_MODE

const overlayPaths = [resolveConfigPath(configPath, undefined)]
const switchPatch = resolveTelemetryPatch(process.env.DSH_TELEMETRY_DISABLED, true)
if (switchPatch !== undefined) {
  await writeFile('./telemetry-switch.patch.yml', JSON.stringify([switchPatch]))
  overlayPaths.push(resolveConfigPath('./telemetry-switch.patch.yml', undefined))
}

try {
  const ctx = await bootProductionProfile({
    binName: 'telemetry-otel-e2e',
    profile: 'headless',
    overlayPaths,
  })
  let acknowledgement: string | undefined
  let sharing: string | undefined
  try {
    await runFixtureTurn(ctx, { task: 'prove telemetry with key sk-e2efixture1234567890' })
    const [agent] = ctx.get('agents')?.roots() ?? []
    if (agent === undefined) throw new Error('session-telemetry-otel driver requires one root agent')
    sharing = ctx.get('sessionTelemetry')?.sharing
    if (process.env.DSH_TELEMETRY_E2E_FEEDBACK !== 'none') {
      const settled = await ctx.commands.execute(agent, '/feedback fixture feedback', [], new AbortController().signal)
      acknowledgement = settled?.result.text
    }
    await runFixtureTurn(ctx, { task: 'post-feedback private suffix' })
    ctx.emit('agent/error', { agent, turn: 2, step: 1, error: new Error('private operational error') })
  } finally {
    await ctx.fiber.dispose()
  }
  await writeFile('./otlp-captures.json', JSON.stringify(captures))
  await writeFile('./telemetry-run.json', JSON.stringify({ collectorUrl, sharing, acknowledgement, outbound }))
} finally {
  unsubscribe('http.client.request.start', observeRequest)
  await new Promise<void>((resolve, reject) => {
    server.close((error) => {
      if (error === undefined) resolve()
      else reject(error)
    })
    server.closeAllConnections()
  })
}
