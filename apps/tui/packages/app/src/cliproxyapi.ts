/** CLIProxyAPI connection setup for Bake's built-in pi-ai route. */
import type { Context } from '@deepseek-ai/cordis'
import { credentialRef } from '@deepseek-ai/dsh-credentials'
import { assertUsableApiKey } from '@deepseek-ai/dsh-llm'
import type {} from '@deepseek-ai/dsh-settings'
import type { AuthorizationPrompt } from '@deepseek-ai/dsh-authorization/types'
import type { TuiCopy } from '@dsh-tui/ui/copy.ts'

export const CLIPROXYAPI_ID = 'cliproxyapi'
export const CLIPROXYAPI_KEY = 'CLIPROXYAPI_API_KEY'
export const CLIPROXYAPI_DEFAULT_URL = 'http://127.0.0.1:8317'
const MAX_CATALOG_BYTES = 4 * 1024 * 1024

/** The wire protocols a login assigns; the route's own is `openai-responses`. */
export type CliProxyApi = 'openai-responses' | 'openai-completions' | 'anthropic-messages'

/** Fields the pi-ai settings route can persist for one discovered model. */
export interface CliProxyModel {
  readonly id: string
  /** Set only where the model leaves the route's `openai-responses`. */
  readonly api?: Exclude<CliProxyApi, 'openai-responses'>
  /** Set with `anthropic-messages`, which joins `/v1/messages` onto the proxy root instead of the route's `/v1`. */
  readonly baseURL?: string
  readonly name: string
  readonly contextWindow?: number
  readonly maxTokens?: number
  readonly input?: ('text' | 'image')[]
  readonly reasoningEfforts?: Record<string, string>
  /** Set on Claude models that take adaptive thinking with an effort level. */
  readonly compat?: { readonly forceAdaptiveThinking: true }
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
  readonly owned_by?: unknown
  readonly visibility?: unknown
  readonly context_window?: unknown
  readonly max_context_window?: unknown
  readonly max_tokens?: unknown
  readonly max_output_tokens?: unknown
  readonly max_completion_tokens?: unknown
  readonly input_modalities?: unknown
  readonly output_modalities?: unknown
  readonly supported_reasoning_levels?: unknown
}

/**
 * Families whose upstreams speak Chat Completions only. CPA serves them on
 * /v1/responses by translating to chat itself, and a strict upstream rejects
 * shapes that translation produces — Kimi answers two parallel tool calls with
 * `tool_call_ids did not have response messages` — so these models skip the
 * translation. An optional `vendor/` or `vendor:` namespace precedes the
 * family, as in `moonshotai/kimi-k3`.
 */
const CHAT_COMPLETIONS_FAMILY = /^(?:[\w.-]+[/:])?(?:kimi-|moonshot-|glm-|qwen|qwq-|deepseek-|minimax-)/i

/**
 * Claude, by id wherever it is hosted. Claude caches a prompt only at the
 * `cache_control` breakpoints a request marks, and CPA's Responses translator
 * carries none, so over Responses every turn re-reads the whole context at
 * full price. Over Anthropic Messages the adapter marks the system prompt,
 * the last tool, and the last user message, and the proxy passes them on.
 */
const ANTHROPIC_FAMILY = /^(?:[\w.-]+[/:])?claude-/i

/**
 * The wire protocol one listed model is served over. OpenAI, Gemini, and Grok
 * stay on Responses, which carries their signed reasoning across turns and
 * which their upstreams cache from `prompt_cache_key` alone.
 * @param id - the model id the proxy lists.
 * @param ownedBy - the listing's `owned_by`, when it names one.
 */
export function cliProxyApi(id: string, ownedBy?: string): CliProxyApi {
  if (ownedBy?.toLowerCase() === 'anthropic' || ANTHROPIC_FAMILY.test(id)) return 'anthropic-messages'
  if (CHAT_COMPLETIONS_FAMILY.test(id)) return 'openai-completions'
  return 'openai-responses'
}

/**
 * Route defaults for a proxy that balances several upstream credentials.
 *
 * - `retryPolicy.backoff.maxDelayMs`: once every credential for a model is cooling
 *   down, CPA and CliRelay answer `429 model_cooldown` with a `reset_seconds`
 *   hint, commonly 30 to 60 seconds. A retry delay capped at the 10-second
 *   default would end the turn instead of waiting it out. Local backoff never
 *   reaches this cap within the default five retries, so only a wait the proxy
 *   asks for gets longer.
 * - `compat.sendSessionAffinityHeaders`: provider prompt caches are per
 *   credential, so a session must stay on one. CPA's sticky routing keys on
 *   `x-session-affinity`, which this sends on the Anthropic Messages and Chat
 *   Completions models; Responses models already carry the session as
 *   `prompt_cache_key`, and CliRelay reads the `x-deepseek-harness-session-id`
 *   every pi-ai request carries.
 */
export const CLIPROXYAPI_ROUTE_DEFAULTS = {
  retryPolicy: { mode: 'normal', backoff: { maxDelayMs: 60_000 } },
  compat: { sendSessionAffinityHeaders: true },
} as const

const positiveInteger = (...values: readonly unknown[]): number | undefined =>
  values.find(value => typeof value === 'number' && Number.isInteger(value) && value > 0) as number | undefined

/**
 * Turn CPA's model listing into the explicit models required by a custom pi-ai route.
 * @param payload - the parsed `/v1/models` reply.
 * @param root - the proxy root, which an Anthropic Messages model is sent to.
 * @returns the chat models, each with the protocol it is served over.
 */
export function cliProxyModels(payload: unknown, root?: string): CliProxyModel[] {
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
    // An image or video generator cannot answer a chat turn.
    if (Array.isArray(entry.output_modalities) && !entry.output_modalities.includes('text')) continue
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
    const api = cliProxyApi(id, typeof entry.owned_by === 'string' ? entry.owned_by : undefined)
    // Adaptive thinking is what takes an effort level. The Claude models that
    // list `xhigh` or `max` accept it; older ones answer it with HTTP 400 and
    // keep budget thinking.
    const adaptive = api === 'anthropic-messages' && efforts.some(level => level === 'xhigh' || level === 'max')
    models.set(id, {
      id,
      ...api === 'openai-responses' ? {} : { api },
      ...api === 'anthropic-messages' && root !== undefined ? { baseURL: root } : {},
      name,
      ...contextWindow === undefined ? {} : { contextWindow },
      ...maxTokens === undefined ? {} : { maxTokens },
      ...input.length === 0 ? {} : { input: input.includes('text') ? input : ['text', ...input] as ('text' | 'image')[] },
      ...efforts.length === 0 ? {} : { reasoningEfforts },
      ...adaptive ? { compat: { forceAdaptiveThinking: true as const } } : {},
    })
  }
  return [...models.values()]
}

/** One thing an in-place upgrade changed on a route an earlier login wrote. */
export type CliProxyRouteChange = 'protocols' | 'adaptive-thinking' | 'retry' | 'affinity'

/** A route upgrade as path edits for the `llm-pi-ai` settings namespace. */
export interface CliProxyRouteUpgradePlan {
  readonly changes: readonly CliProxyRouteChange[]
  readonly ops: readonly { readonly op: 'set', readonly path: readonly string[], readonly value: unknown }[]
}

const isRecord = (value: unknown): value is Readonly<Record<string, unknown>> =>
  value !== null && typeof value === 'object' && !Array.isArray(value)

/**
 * Bring a route an earlier `/login cliproxyapi` wrote up to what the current
 * login writes, from the saved route alone: no network and no key.
 *
 * Every field an older login lacked is derivable from what it saved. A model's
 * protocol follows from its id, Claude's endpoint from the route's URL,
 * adaptive thinking from the efforts the model lists, and the route defaults
 * are constants. Only absent fields are filled; a value the user set, even
 * `false`, is kept, which is also how a default is opted out of. A route whose
 * credential or protocol was changed by hand is not a login's and is left alone.
 * @param saved - the resolved `llm-pi-ai` provider entry for `cliproxyapi`.
 * @returns the edits and what they change, or undefined when nothing needs changing.
 */
export function planCliProxyRouteUpgrade(saved: unknown): CliProxyRouteUpgradePlan | undefined {
  if (!isRecord(saved) || saved['apiKeyEnv'] !== CLIPROXYAPI_KEY || typeof saved['baseURL'] !== 'string'
    || !Array.isArray(saved['models']) || (saved['api'] !== undefined && saved['api'] !== 'openai-responses')) return undefined
  let root: string
  try { root = cliProxyEndpoints(saved['baseURL']).root } catch { return undefined }
  const changes = new Set<CliProxyRouteChange>()
  const models = (saved['models'] as readonly unknown[]).map((entry) => {
    if (!isRecord(entry) || typeof entry['id'] !== 'string') return entry
    let model: Readonly<Record<string, unknown>> = entry
    if (model['api'] === undefined) {
      const api = cliProxyApi(entry['id'])
      if (api !== 'openai-responses') { model = { ...model, api }; changes.add('protocols') }
    }
    if (model['api'] === 'anthropic-messages' && model['baseURL'] === undefined) {
      model = { ...model, baseURL: root }
      changes.add('protocols')
    }
    const efforts = isRecord(model['reasoningEfforts']) ? Object.keys(model['reasoningEfforts']) : []
    const compat = isRecord(model['compat']) ? model['compat'] : undefined
    if (model['api'] === 'anthropic-messages' && efforts.some(level => level === 'xhigh' || level === 'max')
      && compat?.['forceAdaptiveThinking'] === undefined) {
      model = { ...model, compat: { ...compat, forceAdaptiveThinking: true } }
      changes.add('adaptive-thinking')
    }
    return model
  })
  const route = ['providers', CLIPROXYAPI_ID]
  const ops: { op: 'set', path: readonly string[], value: unknown }[] = []
  if (changes.size > 0) ops.push({ op: 'set', path: [...route, 'models'], value: models })
  if (saved['retryPolicy'] === undefined) {
    ops.push({ op: 'set', path: [...route, 'retryPolicy'], value: CLIPROXYAPI_ROUTE_DEFAULTS.retryPolicy })
    changes.add('retry')
  }
  const compat = isRecord(saved['compat']) ? saved['compat'] : undefined
  if (compat?.['sendSessionAffinityHeaders'] === undefined) {
    ops.push({ op: 'set', path: [...route, 'compat', 'sendSessionAffinityHeaders'],
      value: CLIPROXYAPI_ROUTE_DEFAULTS.compat.sendSessionAffinityHeaders })
    changes.add('affinity')
  }
  return ops.length === 0 ? undefined : { changes: [...changes], ops }
}

/** What startup did with a route an earlier login wrote. */
export type CliProxyRouteUpgrade =
  | { readonly kind: 'current' }
  | { readonly kind: 'upgraded', readonly changes: readonly CliProxyRouteChange[] }
  /** The route needs the changes but could not take them in place; a new `/login cliproxyapi` writes them. */
  | { readonly kind: 'relogin', readonly changes: readonly CliProxyRouteChange[], readonly reason: string }

/**
 * Upgrade a saved CLIProxyAPI route in place through the settings service, so
 * an update reaches it without the user signing in again. A settings file that
 * cannot be written, or a write it refuses, leaves the route as it was and
 * reports that the login must be repeated.
 * @param ctx - the settled plugin context.
 * @returns whether the route was already current, was upgraded, or needs a new login.
 */
export async function upgradeCliProxyRoute(ctx: Context): Promise<CliProxyRouteUpgrade> {
  const settings = ctx.get('settings')
  if (settings === undefined) return { kind: 'current' }
  const section = settings.get('llm-pi-ai') as { providers?: Readonly<Record<string, unknown>> } | undefined
  const plan = planCliProxyRouteUpgrade(section?.providers?.[CLIPROXYAPI_ID])
  if (plan === undefined) return { kind: 'current' }
  if (!settings.writable) return { kind: 'relogin', changes: plan.changes, reason: 'the settings file is read-only' }
  try {
    await settings.mutate('llm-pi-ai', plan.ops)
  } catch (error) {
    return { kind: 'relogin', changes: plan.changes, reason: error instanceof Error ? error.message : String(error) }
  }
  return { kind: 'upgraded', changes: plan.changes }
}

/** The localized labels a route-upgrade notice is built from. */
export type CliProxyUpgradeCopy = Pick<TuiCopy, 'cliProxyUpgraded' | 'cliProxyRelogin' | 'cliProxyChangeSeparator'
  | 'cliProxyChangeProtocols' | 'cliProxyChangeAdaptive' | 'cliProxyChangeRetry' | 'cliProxyChangeAffinity'>

/**
 * The startup notice for a route upgrade, naming what changed, or what a new
 * login would change when the route could not be upgraded in place.
 * @param result - what startup did with the saved route.
 * @param copy - localized labels.
 * @returns the notice text, or undefined when the route was already current.
 */
export function cliProxyUpgradeNotice(result: CliProxyRouteUpgrade, copy: CliProxyUpgradeCopy): string | undefined {
  if (result.kind === 'current') return undefined
  const labels: Readonly<Record<CliProxyRouteChange, string>> = {
    'protocols': copy.cliProxyChangeProtocols,
    'adaptive-thinking': copy.cliProxyChangeAdaptive,
    'retry': copy.cliProxyChangeRetry,
    'affinity': copy.cliProxyChangeAffinity,
  }
  const changes = result.changes.map(change => labels[change]).join(copy.cliProxyChangeSeparator)
  return `${result.kind === 'upgraded' ? copy.cliProxyUpgraded : copy.cliProxyRelogin}${changes}`
}

/** Validate a connection before changing either credentials or provider settings. */
export async function fetchCliProxyModels(url: string, apiKey: string, signal: AbortSignal,
  fetcher: typeof fetch = fetch, root?: string): Promise<CliProxyModel[]> {
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
  const models = cliProxyModels(payload, root)
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
  const models = await fetchCliProxyModels(endpoints.models, validKey, signal, fetcher, endpoints.root)
  signal.throwIfAborted()
  const ref = credentialRef(CLIPROXYAPI_KEY)
  const previous = await credentials.resolve(ref)
  await credentials.set(ref, validKey)
  try {
    await settings.mutate('llm-pi-ai', [{ op: 'set', path: ['providers', CLIPROXYAPI_ID], value: {
      displayName: 'CLIProxyAPI', apiKeyEnv: CLIPROXYAPI_KEY,
      api: 'openai-responses', baseURL: endpoints.inference, models,
      ...CLIPROXYAPI_ROUTE_DEFAULTS,
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
