import { createServer, type Server } from 'node:http'
import { mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { pathToFileURL } from 'node:url'
import { afterEach, expect, it, vi } from 'vitest'
import { Context } from '@deepseek-ai/cordis'
import Loader from '@deepseek-ai/cordis-plugin-loader'
import Include from '@deepseek-ai/cordis-plugin-include'
import LlmRuntime from '@deepseek-ai/dsh-llm'
import AgentRegistry from '@deepseek-ai/dsh-agent'
import type { Agent } from '@deepseek-ai/dsh-agent'
import SessionStore, { SessionId } from '@deepseek-ai/dsh-session'
import CommandRuntime from '@deepseek-ai/dsh-commands'
import * as LlmDeepSeek from '@deepseek-ai/dsh-llm-deepseek'

let root: string | undefined
let context: Context | undefined
let server: Server | undefined

afterEach(async () => {
  await context?.fiber.dispose()
  context = undefined
  if (server !== undefined) {
    server.closeAllConnections()
    await new Promise<void>((resolve, reject) => server!.close(error => error === undefined ? resolve() : reject(error)))
  }
  server = undefined
  if (root !== undefined) await rm(root, { recursive: true, force: true })
  root = undefined
  vi.unstubAllEnvs()
})

it('registers /usage through the real DeepSeek composition and returns account credit without model input', async () => {
  const requests: Array<{ path: string; authorization: string | undefined }> = []
  let status = 200
  server = createServer((request, response) => {
    requests.push({ path: request.url ?? '', authorization: request.headers.authorization })
    response.writeHead(status, { 'content-type': 'application/json' })
    response.end(JSON.stringify({ is_available: true, balance_infos: [
      { currency: 'USD', total_balance: '5.25', granted_balance: '1.00', topped_up_balance: '4.25' },
    ] }))
  })
  await new Promise<void>(resolve => server!.listen(0, '127.0.0.1', resolve))
  const address = server.address()
  if (address === null || typeof address === 'string') throw new Error('balance server did not bind')

  root = await mkdtemp(join(tmpdir(), 'dsh-deepseek-usage-'))
  vi.stubEnv('DSH_HOME', root)
  vi.stubEnv('DEEPSEEK_API_KEY', 'test-balance-key')
  const configPath = join(root, 'cordis.yml')
  await writeFile(configPath, [
    "- name: '@deepseek-ai/dsh-session'",
    "- name: '@deepseek-ai/dsh-agent'",
    "- name: '@deepseek-ai/dsh-commands'",
    "- name: '@deepseek-ai/dsh-llm'",
    "- name: '@deepseek-ai/dsh-llm-deepseek'",
    '  config:',
    `    baseURL: http://127.0.0.1:${address.port}/anthropic`,
    '',
  ].join('\n'))
  context = new Context()
  context.baseUrl = pathToFileURL(root).href + '/'
  await context.plugin(Loader)
  context.loader.builtins.include = Include
  const modules = new Map<string, unknown>([
    ['@deepseek-ai/dsh-session', SessionStore],
    ['@deepseek-ai/dsh-agent', AgentRegistry],
    ['@deepseek-ai/dsh-commands', CommandRuntime],
    ['@deepseek-ai/dsh-llm', LlmRuntime],
    ['@deepseek-ai/dsh-llm-deepseek', LlmDeepSeek],
  ])
  context.loader.internal = {
    version: 'v2',
    async import(specifier: string) {
      if (!modules.has(specifier)) throw new Error(`unexpected Loader import: ${specifier}`)
      return modules.get(specifier)
    },
  } as unknown as NonNullable<typeof context.loader.internal>
  await context.loader.create({ name: 'cordis:include', config: { path: pathToFileURL(configPath).href } })
  await context.loader.await()

  const scoped = context.plugin(() => {})
  const id = SessionId('deepseek-usage-agent')
  const session = context.sessions.create(id)
  const agent = { id, options: {}, session, ctx: scoped.ctx } as Agent
  await context.agents.register(agent)
  expect(context.commands.list(agent).map(command => command.name)).toContain('usage')

  const signal = new AbortController().signal
  const success = await context.commands.execute(agent, '/usage', [], signal)
  expect(success?.result).toEqual({ kind: 'success', text:
    'DeepSeek API balance · API calls available\nUSD: 5.25 remaining (1.00 granted, 4.25 topped up)' })
  expect(requests).toEqual([{ path: '/user/balance', authorization: 'Bearer test-balance-key' }])
  expect(session.deriveMessages()).toEqual([])
  expect(JSON.stringify(session.snapshotEvents())).not.toContain('test-balance-key')

  const extra = await context.commands.execute(agent, '/usage extra', [], signal)
  expect(extra?.result).toEqual({ kind: 'error', text: 'Usage: /usage' })
  expect(requests).toHaveLength(1)

  status = 401
  const unauthorized = await context.commands.execute(agent, '/usage', [], signal)
  expect(unauthorized?.result).toEqual({ kind: 'error', text: 'DeepSeek balance request failed (HTTP 401).' })
  expect(requests).toHaveLength(2)

  const provider = [...context.loader.entries()].find(entry => entry.options.name === '@deepseek-ai/dsh-llm-deepseek')
  expect(provider).toBeDefined()
  await provider!.update({ disabled: true })
  await provider!.fiber?.await()
  expect(context.commands.list(agent).map(command => command.name)).not.toContain('usage')
})
