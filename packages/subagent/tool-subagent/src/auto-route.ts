/** Task-aware child route selection through an external router. */

import type { LlmResolvedModelInfo, LlmRuntime } from '@deepseek-ai/dsh-llm'
import { modelRouteKey } from './model-selection.ts'
import type { AllowedModelRoute, DelegationModelRequest } from './model-selection.ts'
import type { SubagentRouteHint, SubagentRouterSettings } from './model-selection-settings.ts'

/** Task text sent to the router; its judge reads only part of it anyway. */
const MAX_TASK_CHARS = 20_000

/**
 * Effort ids whose strength order is known. Adapters list efforts in display
 * order, and an id outside this table cannot be compared, only matched.
 */
const EFFORT_RANK: Readonly<Record<string, number>> = {
  none: 0, off: 0, minimal: 1, low: 2, medium: 3, high: 4, xhigh: 5, max: 6,
}

/** A route the router chose, as the call's model-facing selection fields. */
export interface RoutedDelegation {
  /** Selection fields merged exactly as if the calling model had supplied them. */
  readonly request: DelegationModelRequest
  /** The router's one-line explanation, for logs. */
  readonly reason: string
}

interface RouterAnswer {
  provider?: unknown
  model?: unknown
  reasoning_effort?: unknown
  reason?: unknown
  fallback?: unknown
}

/** What the router is told about one route: this Session's model info and the user's hints. */
interface RouteInfo {
  efforts?: string[]
  default_effort?: string
  context_window?: number
  input_modalities?: string[]
  hints?: { same_as?: string; quality?: string; cost?: string }
}

/**
 * The route's own model info and the user's hints, in the router's terms.
 * A route with no reasoning control sends no efforts, so the router leaves its
 * default alone; one whose info could not be resolved sends only its hints.
 */
function routeInfo(info: LlmResolvedModelInfo | undefined, hint: SubagentRouteHint | undefined): RouteInfo | undefined {
  const hints = hint === undefined ? undefined : {
    ...hint.sameAs === undefined ? {} : { same_as: hint.sameAs },
    ...hint.quality === undefined ? {} : { quality: hint.quality },
    ...hint.cost === undefined ? {} : { cost: hint.cost },
  }
  if (info === undefined) return hints === undefined ? undefined : { hints }
  return {
    efforts: info.reasoning?.efforts.map(effort => effort.id) ?? [],
    ...info.reasoning?.defaultEffort === undefined ? {} : { default_effort: info.reasoning.defaultEffort },
    ...info.context === undefined ? {} : { context_window: info.context.contextWindow },
    ...info.inputModalities === undefined ? {} : { input_modalities: [...info.inputModalities] },
    ...hints === undefined ? {} : { hints },
  }
}

/**
 * The advertised effort for a suggested one: itself when listed, otherwise the
 * listed effort nearest in strength. A tie leans toward less thinking for a
 * `low` suggestion and more for anything harder.
 * @param suggested - Effort id the router suggested.
 * @param info - The chosen route's model info.
 * @returns an advertised effort id, or undefined to keep the model default.
 */
function advertisedEffort(suggested: string, info: LlmResolvedModelInfo): string | undefined {
  const efforts = info.reasoning?.efforts.map(effort => effort.id) ?? []
  if (efforts.includes(suggested as never)) return suggested
  const target = EFFORT_RANK[suggested.toLowerCase()]
  if (target === undefined) return undefined
  const lean = suggested.toLowerCase() === 'low' ? 1 : -1
  let best: { id: string; key: [number, number] } | undefined
  for (const id of efforts) {
    const rank = EFFORT_RANK[id.toLowerCase()]
    if (rank === undefined) continue
    const key: [number, number] = [Math.abs(rank - target), lean * rank]
    if (best === undefined || key[0] < best.key[0] || (key[0] === best.key[0] && key[1] < best.key[1])) {
      best = { id, key }
    }
  }
  return best?.id
}

/** The allowed routes as the router receives them, with each route's resolved model info. */
interface RoutePayload {
  readonly allowedModels: readonly { provider: string; model: string; info?: RouteInfo }[]
  readonly infos: readonly PromiseSettledResult<LlmResolvedModelInfo>[]
}

/**
 * Describe each route in the router's terms. A route whose info cannot be
 * resolved is still offered, with only its hints.
 */
async function routePayload(
  router: SubagentRouterSettings,
  routes: readonly AllowedModelRoute[],
  llm: Pick<LlmRuntime, 'resolveModelInfo'> | undefined,
  signal: AbortSignal,
): Promise<RoutePayload> {
  const hints = new Map((router.hints ?? []).map(hint => [modelRouteKey(hint), hint]))
  const infos = await Promise.allSettled(routes.map(route => llm === undefined
    ? Promise.reject(new Error('no LLM runtime'))
    : llm.resolveModelInfo(route.provider, route.model, signal)))
  signal.throwIfAborted()
  return {
    allowedModels: routes.map((route, index) => {
      const settled = infos[index]
      const info = routeInfo(settled?.status === 'fulfilled' ? settled.value : undefined, hints.get(modelRouteKey(route)))
      return { provider: route.provider, model: route.model, ...info === undefined ? {} : { info } }
    }),
    infos,
  }
}

/**
 * Call one of the router's endpoints: POST a JSON body, or GET without one,
 * with the bearer token when there is one. A refusal carries the router's own
 * reason, which is what a sign-in shows the user.
 */
async function post(router: SubagentRouterSettings, path: string, body: unknown, signal: AbortSignal,
  token: string | undefined): Promise<unknown> {
  const response = await fetch(`${router.url.replace(/\/+$/, '')}${path}`, {
    method: body === undefined ? 'GET' : 'POST',
    headers: {
      ...body === undefined ? {} : { 'content-type': 'application/json' },
      ...token === undefined || token.length === 0 ? {} : { authorization: `Bearer ${token}` },
    },
    ...body === undefined ? {} : { body: JSON.stringify(body) },
    signal: AbortSignal.any([signal, AbortSignal.timeout(router.timeoutMs)]),
  })
  if (response.status === 401 && (token === undefined || token.length === 0)) {
    throw new Error(`router requires a token; sign in from /settings or set ${router.tokenEnv}`)
  }
  if (!response.ok) {
    const detail = await response.json().then(
      (answer: { detail?: unknown }) => typeof answer?.detail === 'string' ? answer.detail : undefined,
      () => undefined)
    throw new RouterRefusal(response.status, detail)
  }
  return response.status === 204 ? undefined : response.json()
}

/** A router's refusal, with the HTTP status and the router's reason when it gave one. */
export class RouterRefusal extends Error {
  constructor(readonly status: number, readonly detail: string | undefined) {
    super(detail === undefined ? `router answered HTTP ${status}` : `router answered HTTP ${status}: ${detail}`)
    this.name = 'RouterRefusal'
  }
}

/**
 * Ask the router to choose one of the Session's allowed routes for a task.
 * Each route travels with its model info (efforts, context window, input
 * types) and the user's hints, so a router that recognizes no model name can
 * still constrain and rank it. The answer must name an allowed route. An
 * effort the chosen model does not advertise becomes the nearest one it does,
 * or is dropped so the model's default applies. An answer the router marks as
 * a fallback, because nothing told the routes apart, is refused so the call
 * keeps its default route.
 * @param router - Router settings read for this delegation.
 * @param task - Delegated task text: its short description and prompt.
 * @param routes - The Session's allowed routes; the only candidates.
 * @param llm - Live LLM runtime, used to describe routes and check the suggested effort.
 * @param signal - Tool-call cancellation signal.
 * @param token - Bearer token for the router, when there is one.
 * @returns the chosen route as selection fields.
 */
export async function routeDelegation(
  router: SubagentRouterSettings,
  task: string,
  routes: readonly AllowedModelRoute[],
  llm: Pick<LlmRuntime, 'resolveModelInfo'> | undefined,
  signal: AbortSignal,
  token?: string,
): Promise<RoutedDelegation> {
  const { allowedModels, infos } = await routePayload(router, routes, llm, signal)
  const answer = await post(router, '/v1/bake/select', {
    task: task.slice(0, MAX_TASK_CHARS),
    allowed_models: allowedModels,
    ...router.priority === undefined ? {} : { priority: router.priority },
  }, signal, token) as RouterAnswer
  const { provider, model } = answer
  const reason = typeof answer.reason === 'string' ? answer.reason : ''
  if (typeof provider !== 'string' || typeof model !== 'string') {
    throw new Error('router answer names no provider and model')
  }
  // The router is outside this Session's authority: its answer must be one of the allowed routes.
  const index = routes.findIndex(route => route.provider === provider && route.model === model)
  if (index < 0) {
    throw new Error(`router chose "${provider}/${model}", which is not allowed for this Session`)
  }
  if (answer.fallback === true) {
    throw new Error(`router could not tell the allowed routes apart: ${reason}`)
  }
  const suggested = typeof answer.reasoning_effort === 'string' ? answer.reasoning_effort : undefined
  const settled = infos[index]
  const effort = suggested !== undefined && settled?.status === 'fulfilled'
    ? advertisedEffort(suggested, settled.value)
    : undefined
  return {
    request: { provider, model, ...effort === undefined ? {} : { reasoning_effort: effort } },
    reason,
  }
}

/** What the router knows about one route: how it recognized the model and what it scores it with. */
export interface RouterRouteView {
  readonly provider: string
  readonly model: string
  /** The benchmarked model the router matched, by the route's own name or its `sameAs` hint. */
  readonly profile?: string
  readonly matchedBy?: 'name' | 'same_as'
  /**
   * Whether the router has quality evidence for the route. When no allowed
   * route has any, a routed delegation keeps its default route.
   */
  readonly ranked: boolean
  /** Overall quality from 0 to 1. */
  readonly quality?: number
  readonly qualitySource?: 'benchmarks' | 'hint'
  /** Blended USD per million tokens. */
  readonly price?: number
  readonly priceSource?: 'catalog' | 'hint'
}

const oneOf = <T extends string>(value: unknown, allowed: readonly T[]): T | undefined =>
  allowed.includes(value as T) ? value as T : undefined

/**
 * Ask the router how it recognizes each route, sending exactly what a routed
 * delegation sends about them.
 * @param router - Router settings; routing need not be on.
 * @param routes - The routes to describe.
 * @param llm - Live LLM runtime, used to describe routes.
 * @param signal - The caller's lifetime.
 * @param token - Bearer token for the router, when there is one.
 * @returns one view per route, in the routes' order.
 */
export async function describeRoutes(
  router: SubagentRouterSettings,
  routes: readonly AllowedModelRoute[],
  llm: Pick<LlmRuntime, 'resolveModelInfo'> | undefined,
  signal: AbortSignal,
  token?: string,
): Promise<readonly RouterRouteView[]> {
  if (routes.length === 0) return []
  const { allowedModels } = await routePayload(router, routes, llm, signal)
  const answer = await post(router, '/v1/bake/routes', { allowed_models: allowedModels }, signal, token) as { routes?: unknown }
  const views = Array.isArray(answer.routes) ? answer.routes as Record<string, unknown>[] : []
  // The router is outside this Session's authority; only views of the routes asked about are kept.
  return routes.map((route) => {
    const view = views.find(entry => entry?.['provider'] === route.provider && entry?.['model'] === route.model) ?? {}
    const profile = typeof view['profile'] === 'string' ? view['profile'] : undefined
    const matchedBy = oneOf(view['matched_by'], ['name', 'same_as'] as const)
    const qualitySource = oneOf(view['quality_source'], ['benchmarks', 'hint'] as const)
    const priceSource = oneOf(view['price_source'], ['catalog', 'hint'] as const)
    return {
      provider: route.provider, model: route.model, ranked: view['ranked'] === true,
      ...profile === undefined ? {} : { profile },
      ...matchedBy === undefined ? {} : { matchedBy },
      ...typeof view['quality'] === 'number' ? { quality: view['quality'] } : {},
      ...qualitySource === undefined ? {} : { qualitySource },
      ...typeof view['price'] === 'number' ? { price: view['price'] } : {},
      ...priceSource === undefined ? {} : { priceSource },
    }
  })
}

/**
 * Ask the router to email a one-time sign-in code. The first verified code for
 * an address registers it.
 * @param router - Router settings; routing need not be on.
 * @param email - The address to sign in with.
 * @param signal - The caller's lifetime.
 */
export async function requestSignInCode(router: SubagentRouterSettings, email: string, signal: AbortSignal): Promise<void> {
  await post(router, '/auth/email/start', { email }, signal, undefined).catch(asSignInError)
}

/** A sign-in refusal says why in the router's words, which is what the user needs to read. */
function asSignInError(error: unknown): never {
  if (error instanceof RouterRefusal && error.detail !== undefined) throw new Error(error.detail, { cause: error })
  throw error
}

/**
 * Trade an emailed code for a new router token, labelled `bake`.
 * @param router - Router settings; routing need not be on.
 * @param email - The address the code was sent to.
 * @param code - The code as the user typed it.
 * @param signal - The caller's lifetime.
 * @returns the token, shown once, and the account's address.
 */
export async function redeemSignInCode(router: SubagentRouterSettings, email: string, code: string,
  signal: AbortSignal): Promise<{ readonly token: string; readonly email: string }> {
  const answer = await post(router, '/auth/email/verify', { email, code, label: 'bake' }, signal, undefined)
    .catch(asSignInError) as { token?: unknown; email?: unknown }
  if (typeof answer?.token !== 'string' || answer.token.length === 0) throw new Error('router issued no token')
  return { token: answer.token, email: typeof answer.email === 'string' ? answer.email : email }
}

/**
 * The account a router token belongs to.
 * @param router - Router settings; routing need not be on.
 * @param token - The token to ask about.
 * @param signal - The caller's lifetime.
 * @returns the account's address, or undefined when the router does not accept the token as an account's.
 */
export async function routerAccount(router: SubagentRouterSettings, token: string, signal: AbortSignal): Promise<string | undefined> {
  try {
    const answer = await post(router, '/auth/me', undefined, signal, token) as { email?: unknown }
    return typeof answer?.email === 'string' ? answer.email : undefined
  } catch (error) {
    if (error instanceof RouterRefusal && error.status === 401) return undefined
    throw error
  }
}

/**
 * Revoke a router token at the router.
 * @param router - Router settings; routing need not be on.
 * @param token - The token to revoke.
 * @param signal - The caller's lifetime.
 */
export async function revokeRouterToken(router: SubagentRouterSettings, token: string, signal: AbortSignal): Promise<void> {
  await post(router, '/auth/logout', {}, signal, token)
}
