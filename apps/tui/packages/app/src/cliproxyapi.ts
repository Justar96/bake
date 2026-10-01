/** CLIProxyAPI connection setup for Bake's built-in pi-ai route. */
import type { Context } from '@deepseek-ai/cordis'
import { credentialRef } from '@deepseek-ai/dsh-credentials'
import { normalizeApiKey } from '@deepseek-ai/dsh-llm'
import type {} from '@deepseek-ai/dsh-settings'
import type {} from '@deepseek-ai/dsh-agent-default-model'
import type { TuiCopy } from '@dsh-tui/ui/copy.ts'
import type { LoginPrompt } from './login.ts'

export const CLIPROXYAPI_ID = 'cliproxyapi'
export const CLIPROXYAPI_KEY = 'CLIPROXYAPI_API_KEY'
export const CLIPROXYAPI_DEFAULT_URL = 'http://127.0.0.1:8317'
const MAX_CATALOG_BYTES = 4 * 1024 * 1024
const CATALOG_TIMEOUT_MS = 15_000

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

/**
 * Why a proxy did not validate, and so which field a sign-in asks for again:
 * a refused key is the key's fault, everything else the address's.
 */
export type CliProxyCheckFailure =
  | { readonly reason: 'unreachable', readonly host: string, readonly detail: string }
  | { readonly reason: 'timeout', readonly host: string }
  | { readonly reason: 'redirect' }
  | { readonly reason: 'rejected', readonly status: number }
  | { readonly reason: 'not-found' }
  | { readonly reason: 'status', readonly status: number }
  | { readonly reason: 'not-proxy' }
  | { readonly reason: 'too-large' }
  | { readonly reason: 'empty' }

/** A proxy that did not validate. The message is English for logs; a surface words `failure` itself. */
export class CliProxyCheckError extends Error {
  constructor(readonly failure: CliProxyCheckFailure, message: string, options?: ErrorOptions) {
    super(message, options)
    this.name = 'CliProxyCheckError'
  }

  /** The field a sign-in asks for again. */
  get field(): 'url' | 'key' { return this.failure.reason === 'rejected' ? 'key' : 'url' }
}

/**
 * Validate a connection before changing either credentials or provider settings.
 * @param url - the proxy's model-list URL.
 * @param apiKey - the key to check.
 * @param signal - the caller's lifetime; its abort propagates as itself.
 * @param fetcher - the HTTP client.
 * @param root - the proxy root, which an Anthropic Messages model is sent to.
 * @returns the chat models the proxy lists.
 * @throws CliProxyCheckError when the proxy cannot be reached or does not validate.
 */
export async function fetchCliProxyModels(url: string, apiKey: string, signal: AbortSignal,
  fetcher: typeof fetch = fetch, root?: string): Promise<CliProxyModel[]> {
  const host = new URL(url).host
  let response: Response
  try {
    response = await fetcher(url, {
      headers: { Authorization: `Bearer ${apiKey}`, Accept: 'application/json' },
      redirect: 'error',
      signal: AbortSignal.any([signal, AbortSignal.timeout(CATALOG_TIMEOUT_MS)]),
    })
  } catch (error) {
    if (signal.aborted) throw error
    if (error instanceof Error && error.name === 'TimeoutError') {
      throw new CliProxyCheckError({ reason: 'timeout', host }, `CLIProxyAPI at ${host} did not answer within 15 seconds`, { cause: error })
    }
    // undici reports every transport failure as `fetch failed`; the cause says which.
    const cause = error instanceof Error && error.cause instanceof Error ? error.cause as Error & { code?: unknown } : undefined
    if (cause !== undefined && /redirect/iu.test(cause.message)) {
      throw new CliProxyCheckError({ reason: 'redirect' }, `CLIProxyAPI at ${host} redirected the model request`, { cause: error })
    }
    const detail = typeof cause?.code === 'string' ? cause.code : cause?.message ?? (error instanceof Error ? error.message : String(error))
    throw new CliProxyCheckError({ reason: 'unreachable', host, detail }, `CLIProxyAPI at ${host} is unreachable: ${detail}`, { cause: error })
  }
  if (response.status !== 200) {
    await response.body?.cancel().catch(() => {})
    const failure: CliProxyCheckFailure = response.status === 401 || response.status === 403 ? { reason: 'rejected', status: response.status }
      : response.status === 404 ? { reason: 'not-found' } : { reason: 'status', status: response.status }
    throw new CliProxyCheckError(failure, `CLIProxyAPI model request failed (HTTP ${response.status})`)
  }
  const length = Number(response.headers.get('content-length'))
  if (length > MAX_CATALOG_BYTES) {
    await response.body?.cancel()
    throw new CliProxyCheckError({ reason: 'too-large' }, 'CLIProxyAPI model list is too large')
  }
  const reader = response.body?.getReader()
  if (reader === undefined) throw new CliProxyCheckError({ reason: 'not-proxy' }, 'CLIProxyAPI returned an empty model response')
  const chunks: Uint8Array[] = []
  let size = 0
  try {
    for (;;) {
      const { done, value } = await reader.read()
      if (done) break
      size += value.byteLength
      if (size > MAX_CATALOG_BYTES) throw new CliProxyCheckError({ reason: 'too-large' }, 'CLIProxyAPI model list is too large')
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
  catch { throw new CliProxyCheckError({ reason: 'not-proxy' }, 'CLIProxyAPI returned invalid model JSON') }
  let models: CliProxyModel[]
  try { models = cliProxyModels(payload, root) }
  catch (error) { throw new CliProxyCheckError({ reason: 'not-proxy' }, 'CLIProxyAPI returned an invalid model list', { cause: error }) }
  if (models.length === 0) throw new CliProxyCheckError({ reason: 'empty' }, 'CLIProxyAPI returned no selectable models')
  return models
}

/** The labels CLIProxyAPI setup shows. */
export type CliProxySetupCopy = Pick<TuiCopy, 'signInTitle' | 'loginEmpty' | 'keyInvalid' | 'keyStoredLocally'
  | 'cliProxyUrl' | 'cliProxyUrlHint' | 'cliProxyKey' | 'cliProxyChecking' | 'cliProxyBadUrl' | 'cliProxyUnreachable'
  | 'cliProxyTimeout' | 'cliProxyRedirect' | 'cliProxyRejected' | 'cliProxyNotFound' | 'cliProxyStatus'
  | 'cliProxyNotProxy' | 'cliProxyTooLarge' | 'cliProxyEmpty'>

/**
 * Word a failed check for the field that asks again.
 * @param failure - why the proxy did not validate.
 * @param copy - localized labels.
 * @returns one line naming what went wrong.
 */
export function cliProxyFailureText(failure: CliProxyCheckFailure, copy: CliProxySetupCopy): string {
  switch (failure.reason) {
    case 'unreachable': return `${copy.cliProxyUnreachable} ${failure.host} (${failure.detail})`
    case 'timeout': return `${copy.cliProxyTimeout} ${failure.host}`
    case 'redirect': return copy.cliProxyRedirect
    case 'rejected': return `${copy.cliProxyRejected} (HTTP ${failure.status})`
    case 'not-found': return `${copy.cliProxyNotFound} (HTTP 404)`
    case 'status': return `${copy.cliProxyStatus} HTTP ${failure.status}`
    case 'not-proxy': return copy.cliProxyNotProxy
    case 'too-large': return copy.cliProxyTooLarge
    case 'empty': return copy.cliProxyEmpty
  }
}

/**
 * Ask for the proxy's address and key, check them against its model list,
 * then install the route for the next request.
 *
 * A refused answer asks again in the same panel, with the reason: a bad
 * address or an unreachable proxy returns to the address with what was typed,
 * and a rejected key returns to the key. Only an unexpected error ends the
 * setup; a declined prompt ends it as the prompt's own rejection.
 *
 * @param ctx - the settled plugin context.
 * @param prompt - asks one field; rejects when the user declines.
 * @param signal - command cancellation lifetime.
 * @param copy - localized labels.
 * @param fetcher - the HTTP client.
 * @param checking - told the proxy host while its model list is being read.
 * @returns how many models the route now offers.
 */
export async function configureCliProxyApi(ctx: Context, prompt: (question: LoginPrompt) => Promise<string>,
  signal: AbortSignal, copy: CliProxySetupCopy, fetcher: typeof fetch = fetch,
  checking: (host: string) => void = () => {}): Promise<number> {
  const credentials = ctx.get('credentials')
  const settings = ctx.get('settings')
  if (credentials === undefined || settings === undefined) throw new Error('CLIProxyAPI setup requires credentials and settings')
  const configured = settings.get('llm-pi-ai') as { providers?: Record<string, { baseURL?: unknown }> } | undefined
  const savedBaseURL = configured?.providers?.[CLIPROXYAPI_ID]?.baseURL
  const defaultURL = typeof savedBaseURL === 'string'
    ? cliProxyEndpoints(savedBaseURL).root : CLIPROXYAPI_DEFAULT_URL
  const title = `${copy.signInTitle} \u00b7 CLIProxyAPI`
  let step: 'url' | 'key' = 'url'
  let typedURL = ''
  let typedKey = ''
  let urlError: string | undefined
  let keyError: string | undefined
  let endpoints = cliProxyEndpoints(defaultURL)
  let validKey = ''
  let models: CliProxyModel[]
  for (;;) {
    if (step === 'url') {
      typedURL = (await prompt({ kind: 'text', title, message: copy.cliProxyUrl, step: { index: 1, count: 2 },
        fallback: defaultURL, hint: copy.cliProxyUrlHint,
        ...urlError === undefined ? {} : { error: urlError }, ...typedURL === '' ? {} : { initial: typedURL } })).trim()
      signal.throwIfAborted()
      try { endpoints = cliProxyEndpoints(typedURL === '' ? defaultURL : typedURL) }
      catch { urlError = copy.cliProxyBadUrl; continue }
      urlError = undefined
      step = 'key'
    }
    const host = new URL(endpoints.root).host
    typedKey = (await prompt({ kind: 'secret', title, message: copy.cliProxyKey, step: { index: 2, count: 2 },
      hint: `${host} \u00b7 ${copy.keyStoredLocally}`,
      ...keyError === undefined ? {} : { error: keyError }, ...typedKey === '' ? {} : { initial: typedKey } })).trim()
    signal.throwIfAborted()
    const checked = normalizeApiKey(typedKey)
    if (!checked.ok) { keyError = checked.reason === 'empty' ? copy.loginEmpty : copy.keyInvalid; typedKey = ''; continue }
    keyError = undefined
    validKey = checked.value
    checking(host)
    try {
      models = await fetchCliProxyModels(endpoints.models, validKey, signal, fetcher, endpoints.root)
    } catch (error) {
      if (signal.aborted || !(error instanceof CliProxyCheckError)) throw error
      const text = cliProxyFailureText(error.failure, copy)
      // A rejected key is asked for fresh; an address problem keeps the key for the next check.
      if (error.field === 'key') { keyError = text; typedKey = '' } else { urlError = text; step = 'url' }
      continue
    }
    break
  }
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

/** A model list with absent and empty fields dropped and keys sorted, for comparing a saved list with a fetched one. */
/** Capabilities a listing can omit for a model that still has them. */
const LISTED_CAPABILITIES = ['contextWindow', 'maxTokens', 'input', 'reasoningEfforts', 'compat'] as const

/**
 * Fill the capabilities a listed model lacks from its saved entry. The
 * protocol, endpoint, and name always follow the listing; adaptive thinking
 * travels with the saved efforts it was derived from.
 * @param listed - the model as the proxy lists it now.
 * @param saved - the same id's saved entry, if any.
 * @returns the listed model, completed from the saved one.
 */
function withSavedCapabilities(listed: CliProxyModel, saved: Readonly<Record<string, unknown>> | undefined): CliProxyModel {
  if (saved === undefined) return listed
  const filled: Record<string, unknown> = {}
  for (const field of LISTED_CAPABILITIES) {
    const value = saved[field]
    const empty = value === undefined || (Array.isArray(value) && value.length === 0)
      || (isRecord(value) && Object.keys(value).length === 0)
    if (listed[field] !== undefined || empty) continue
    if (field === 'compat') {
      // A listing with its own efforts already derived its own adaptive thinking.
      if (listed.reasoningEfforts === undefined && isRecord(value) && value['forceAdaptiveThinking'] === true) {
        filled[field] = { forceAdaptiveThinking: true }
      }
      continue
    }
    filled[field] = value
  }
  return Object.keys(filled).length === 0 ? listed : { ...listed, ...filled } as CliProxyModel
}

function canonical(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(canonical)
  if (!isRecord(value)) return value
  return Object.fromEntries(Object.keys(value).sort().flatMap((key) => {
    const field = canonical(value[key])
    const empty = field === undefined || (Array.isArray(field) && field.length === 0)
      || (isRecord(field) && Object.keys(field).length === 0)
    return empty ? [] : [[key, field]]
  }))
}

/**
 * Bring a saved CLIProxyAPI route's models up to what the proxy lists now, so
 * a model it started serving after `/login cliproxyapi` can be chosen without
 * signing in again. Only a route a current login wrote is refreshed: one an
 * earlier release wrote waits for {@link upgradeCliProxyRoute}, and one whose
 * credential or protocol was changed by hand is not a login's.
 *
 * A model the proxy no longer lists is dropped, as a new login would drop it,
 * unless `keep` names it: a transient gap in the proxy's accounts must not
 * take away the model a session is running on. A listed model keeps the
 * saved capabilities its listing leaves out: a listing that stops reporting a
 * model's effort levels or limits does not mean the model lost them, and
 * dropping its efforts would refuse every request that names one.
 *
 * @param ctx - the settled plugin context.
 * @param signal - bounds the request; its abort propagates as itself.
 * @param keep - model ids to retain when the proxy stops listing them.
 * @param fetcher - the HTTP client.
 * @returns whether the saved list changed.
 * @throws CliProxyCheckError when the proxy cannot be read; the saved list is left as it was.
 */
export async function refreshCliProxyModels(ctx: Context, signal: AbortSignal, keep: readonly string[] = [],
  fetcher: typeof fetch = fetch): Promise<boolean> {
  const settings = ctx.get('settings')
  const credentials = ctx.get('credentials')
  if (settings === undefined || credentials === undefined || !settings.writable) return false
  const section = settings.get('llm-pi-ai') as { providers?: Readonly<Record<string, unknown>> } | undefined
  const saved = section?.providers?.[CLIPROXYAPI_ID]
  if (!isRecord(saved) || saved['apiKeyEnv'] !== CLIPROXYAPI_KEY || typeof saved['baseURL'] !== 'string'
    || !Array.isArray(saved['models']) || (saved['api'] !== undefined && saved['api'] !== 'openai-responses')
    || planCliProxyRouteUpgrade(saved) !== undefined) return false
  let endpoints: ReturnType<typeof cliProxyEndpoints>
  try { endpoints = cliProxyEndpoints(saved['baseURL']) } catch { return false }
  const key = await credentials.resolve(credentialRef(CLIPROXYAPI_KEY))
  if (key === undefined) return false
  const listed = await fetchCliProxyModels(endpoints.models, key.value, signal, fetcher, endpoints.root)
  signal.throwIfAborted()
  const ids = new Set(listed.map(model => model.id))
  const savedById = new Map((saved['models'] as readonly unknown[]).flatMap(entry =>
    isRecord(entry) && typeof entry['id'] === 'string' ? [[entry['id'], entry] as const] : []))
  const kept = [...savedById.values()].filter(entry => keep.includes(entry['id'] as string) && !ids.has(entry['id'] as string))
  const models = [...listed.map(model => withSavedCapabilities(model, savedById.get(model.id))), ...kept]
  if (JSON.stringify(canonical(models)) === JSON.stringify(canonical(saved['models']))) return false
  await settings.mutate('llm-pi-ai', [{ op: 'set', path: ['providers', CLIPROXYAPI_ID, 'models'], value: models }])
  return true
}

/**
 * The CLIProxyAPI models something already uses: the session's model, the
 * new-session default, and the subagent allow-list, which a refresh keeps.
 * @param ctx - the settled plugin context.
 * @param current - the running session's model, when it has one.
 * @returns their model ids on the `cliproxyapi` route.
 */
export function cliProxyModelsInUse(ctx: Context, current?: { readonly provider: string, readonly model: string }): string[] {
  const allowed = (ctx.get('settings')?.get('subagent-model-selection') as { allowedModels?: unknown } | undefined)?.allowedModels
  const routes: unknown[] = [current, ctx.get('agentDefaultModel')?.currentSelection(), ...Array.isArray(allowed) ? allowed : []]
  return [...new Set(routes.flatMap(route => isRecord(route) && route['provider'] === CLIPROXYAPI_ID
    && typeof route['model'] === 'string' ? [route['model']] : []))]
}
