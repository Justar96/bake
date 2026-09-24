/** CLIProxyAPI connection setup for Bake's built-in pi-ai route. */
import type { Context } from '@deepseek-ai/cordis'
import { credentialRef } from '@deepseek-ai/dsh-credentials'
import { assertUsableApiKey } from '@deepseek-ai/dsh-llm'
import type {} from '@deepseek-ai/dsh-settings'
import type { AuthorizationPrompt } from '@deepseek-ai/dsh-authorization/types'

export const CLIPROXYAPI_ID = 'cliproxyapi'
export const CLIPROXYAPI_KEY = 'CLIPROXYAPI_API_KEY'
export const CLIPROXYAPI_DEFAULT_URL = 'http://127.0.0.1:8317'
const MAX_CATALOG_BYTES = 4 * 1024 * 1024

/** Fields the pi-ai settings route can persist for one discovered model. */
export interface CliProxyModel {
  readonly id: string
  readonly name: string
  readonly contextWindow?: number
  readonly maxTokens?: number
  readonly input?: ('text' | 'image')[]
  readonly reasoningEfforts?: Record<string, string>
}

/** The proxy accepts a root URL, a /v1 URL, or its native /backend-api URL. */
export function cliProxyEndpoints(input: string): { readonly root: string, readonly models: string, readonly inference: string } {
  const raw = input.trim()
  if (raw === '') throw new Error('CLIProxyAPI URL is empty')
  const url = new URL(/^https?:\/\//i.test(raw) ? raw : `http://${raw}`)
  if (!['http:', 'https:'].includes(url.protocol) || url.username !== '' || url.password !== '' || url.search !== '' || url.hash !== '') {
    throw new Error('CLIProxyAPI URL must be an HTTP(S) address without credentials, query, or fragment')
  }
  const path = url.pathname.replace(/\/+$/, '').replace(/\/(?:v1|backend-api)$/, '')
  const root = `${url.origin}${path}`
  return { root, models: `${root}/v1/models?client_version=pi`, inference: `${root}/v1` }
}

interface CatalogModel {
  readonly slug?: unknown
  readonly id?: unknown
  readonly display_name?: unknown
  readonly name?: unknown
  readonly visibility?: unknown
  readonly context_window?: unknown
  readonly max_context_window?: unknown
  readonly max_tokens?: unknown
  readonly max_output_tokens?: unknown
  readonly max_completion_tokens?: unknown
  readonly input_modalities?: unknown
  readonly supported_reasoning_levels?: unknown
}

const positiveInteger = (...values: readonly unknown[]): number | undefined =>
  values.find(value => typeof value === 'number' && Number.isInteger(value) && value > 0) as number | undefined

/** Turn CPA's model listing into the explicit models required by a custom pi-ai route. */
export function cliProxyModels(payload: unknown): CliProxyModel[] {
  const record = payload !== null && typeof payload === 'object' && !Array.isArray(payload)
    ? payload as { models?: unknown, data?: unknown } : undefined
  const entries = Array.isArray(payload) ? payload : Array.isArray(record?.models) ? record.models : record?.data
  if (!Array.isArray(entries)) throw new Error('CLIProxyAPI returned an invalid model list')
  const models = new Map<string, CliProxyModel>()
  for (const value of entries) {
    if (value === null || typeof value !== 'object' || Array.isArray(value)) continue
    const entry = value as CatalogModel
    const id = typeof entry.slug === 'string' && entry.slug.trim() !== '' ? entry.slug.trim()
      : typeof entry.id === 'string' ? entry.id.trim() : ''
    if (id === '' || (typeof entry.visibility === 'string' && entry.visibility.toLowerCase() === 'hide')) continue
    const name = typeof entry.display_name === 'string' && entry.display_name.trim() !== '' ? entry.display_name.trim()
      : typeof entry.name === 'string' && entry.name.trim() !== '' ? entry.name.trim() : id
    const contextWindow = positiveInteger(entry.context_window, entry.max_context_window)
    const maxTokens = positiveInteger(entry.max_tokens, entry.max_output_tokens, entry.max_completion_tokens)
    const input = Array.isArray(entry.input_modalities)
      ? entry.input_modalities.filter((item): item is 'text' | 'image' => item === 'text' || item === 'image') : []
    const efforts = Array.isArray(entry.supported_reasoning_levels)
      ? entry.supported_reasoning_levels.map(level => typeof level === 'string' ? level
        : level !== null && typeof level === 'object' ? (level as { effort?: unknown }).effort : undefined)
        .filter((level): level is string => typeof level === 'string' && ['minimal', 'low', 'medium', 'high', 'xhigh', 'max'].includes(level)) : []
    const reasoningEfforts = Object.fromEntries([...new Set(efforts)].map(level => [level, level]))
    models.set(id, {
      id, name,
      ...contextWindow === undefined ? {} : { contextWindow },
      ...maxTokens === undefined ? {} : { maxTokens },
      ...input.length === 0 ? {} : { input: input.includes('text') ? input : ['text', ...input] as ('text' | 'image')[] },
      ...efforts.length === 0 ? {} : { reasoningEfforts },
    })
  }
  return [...models.values()]
}

/** Validate a connection before changing either credentials or provider settings. */
export async function fetchCliProxyModels(url: string, apiKey: string, signal: AbortSignal,
  fetcher: typeof fetch = fetch): Promise<CliProxyModel[]> {
  const response = await fetcher(url, {
    headers: { Authorization: `Bearer ${apiKey}`, Accept: 'application/json' },
    redirect: 'error',
    signal: AbortSignal.any([signal, AbortSignal.timeout(15_000)]),
  })
  if (response.status !== 200) throw new Error(`CLIProxyAPI model request failed (HTTP ${response.status})`)
  const length = Number(response.headers.get('content-length'))
  if (length > MAX_CATALOG_BYTES) {
    await response.body?.cancel()
    throw new Error('CLIProxyAPI model list is too large')
  }
  const reader = response.body?.getReader()
  if (reader === undefined) throw new Error('CLIProxyAPI returned an empty model response')
  const chunks: Uint8Array[] = []
  let size = 0
  try {
    for (;;) {
      const { done, value } = await reader.read()
      if (done) break
      size += value.byteLength
      if (size > MAX_CATALOG_BYTES) throw new Error('CLIProxyAPI model list is too large')
      chunks.push(value)
    }
  } finally {
    await reader.cancel().catch(() => {})
  }
  const body = new Uint8Array(size)
  let offset = 0
  for (const chunk of chunks) { body.set(chunk, offset); offset += chunk.byteLength }
  let payload: unknown
  try { payload = JSON.parse(new TextDecoder().decode(body)) as unknown }
  catch { throw new Error('CLIProxyAPI returned invalid model JSON') }
  const models = cliProxyModels(payload)
  if (models.length === 0) throw new Error('CLIProxyAPI returned no selectable models')
  return models
}

/** Prompt for both connection fields, then install the route for the next request. */
export async function configureCliProxyApi(ctx: Context, prompt: (question: AuthorizationPrompt) => Promise<string>,
  signal: AbortSignal, labels: { readonly url: string, readonly key: string }, fetcher: typeof fetch = fetch): Promise<number> {
  const credentials = ctx.get('credentials')
  const settings = ctx.get('settings')
  if (credentials === undefined || settings === undefined) throw new Error('CLIProxyAPI setup requires credentials and settings')
  const configured = settings.get('llm-pi-ai') as { providers?: Record<string, { baseURL?: unknown }> } | undefined
  const savedBaseURL = configured?.providers?.[CLIPROXYAPI_ID]?.baseURL
  const defaultURL = typeof savedBaseURL === 'string'
    ? cliProxyEndpoints(savedBaseURL).root : CLIPROXYAPI_DEFAULT_URL
  const input = await prompt({ kind: 'text', message: `1/2 · ${labels.url} [${defaultURL}]` })
  signal.throwIfAborted()
  const endpoints = cliProxyEndpoints(input.trim() === '' ? defaultURL : input)
  const apiKey = (await prompt({ kind: 'secret', message: `2/2 · ${labels.key}` })).trim()
  signal.throwIfAborted()
  if (apiKey === '') throw new Error('CLIProxyAPI API key is empty')
  const validKey = assertUsableApiKey(apiKey, CLIPROXYAPI_ID, CLIPROXYAPI_KEY)
  const models = await fetchCliProxyModels(endpoints.models, validKey, signal, fetcher)
  signal.throwIfAborted()
  const ref = credentialRef(CLIPROXYAPI_KEY)
  const previous = await credentials.resolve(ref)
  await credentials.set(ref, validKey)
  try {
    await settings.mutate('llm-pi-ai', [{ op: 'set', path: ['providers', CLIPROXYAPI_ID], value: {
      displayName: 'CLIProxyAPI', apiKeyEnv: CLIPROXYAPI_KEY,
      api: 'openai-responses', baseURL: endpoints.inference, models,
    } }])
  } catch (error) {
    try {
      if (previous === undefined) await credentials.unset(ref)
      else await credentials.set(ref, previous.value)
    } catch (rollback) {
      throw new AggregateError([error, rollback], 'CLIProxyAPI setup failed and the previous API key could not be restored')
    }
    throw error
  }
  return models.length
}
