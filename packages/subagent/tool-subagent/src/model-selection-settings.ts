/** Host-owned opt-in setting for model-selectable subagent delegation. */

import { Context, Service } from '@deepseek-ai/cordis'
import z from '@deepseek-ai/schemastery'
import type {} from 'bake-settings'
import type { LlmRuntime } from '@deepseek-ai/dsh-llm'
import { credentialRef, isCredentialRefName, type CredentialProvider } from 'bake-credentials'
import {
  describeRoutes,
  redeemSignInCode,
  requestSignInCode,
  revokeRouterToken,
  routerAccount,
  type RouterRouteView,
} from './auto-route.ts'
import {
  AllowedModelRouteSchema,
  assertAllowedModelRoutes,
  type AllowedModelRoute,
} from './model-selection.ts'

declare module '@deepseek-ai/cordis' {
  interface Context {
    /** User preference sampled when a new Session receives delegation tools. */
    subagentModelSelection: SubagentModelSelectionConfig
  }
}

/** User-settings section for model-selectable subagent delegation. */
export const SUBAGENT_MODEL_SELECTION_SETTINGS_NAMESPACE = 'subagent-model-selection'

/** The hosted `ing` router, used unless the user names another. */
export const DEFAULT_ROUTER_URL = 'https://ing.gissx.org'

/**
 * Task router that picks a child route from the Session's allowed models when
 * a delegation call names none. It speaks the `ing` router's `/v1/bake/select`
 * protocol. It is a beta, off until the user turns it on, and sends each
 * routed delegation's task text and the allowed model names to `url`; the
 * user's own providers still run every model.
 */
export interface SubagentRouterSettings {
  /** Whether delegations that name no model ask the router for one. */
  enabled: boolean
  /** Router base URL; the hosted `ing` by default. */
  url: string
  /**
   * Credential reference holding the router's bearer token. Signing in from
   * `/settings` stores it; an environment variable of that name wins. Unset
   * sends none.
   */
  tokenEnv: string
  /** Milliseconds to wait for a route before the call uses its default route. */
  timeoutMs: number
  /** Trade-off the router applies instead of inferring one from the task text; unset lets it infer. */
  priority?: SubagentRouterPriority
  /** What the user declares about allowed routes the router's sources may not know. */
  hints?: SubagentRouteHint[]
}

/** A trade-off the user states for routed delegations. */
export type SubagentRouterPriority = 'quality' | 'cost' | 'speed' | 'balanced'

/**
 * The user's declarations about one allowed route, sent to the router with it.
 * They matter most for a route the router cannot recognize: a gateway alias,
 * a local model, or a fine-tune.
 */
export interface SubagentRouteHint {
  /** Registered LLM provider id of the route. */
  provider: string
  /** Provider-owned exact model id of the route. */
  model: string
  /** A model the router knows that this route serves, e.g. `claude-opus-4.5` behind a gateway alias. */
  sameAs?: string
  /** Quality tier standing in for benchmarks when the router has none for the route. */
  quality?: 'low' | 'medium' | 'high' | 'frontier'
  /** Cost tier replacing the router's price, e.g. `free` for a local model. */
  cost?: 'free' | 'low' | 'medium' | 'high'
}

/** Whether Bake holds a router token, and whether signing in can store one. */
export interface RouterTokenStatus {
  /** Credential reference the token is stored under. */
  readonly tokenEnv: string
  /** Whether a token resolves. */
  readonly configured: boolean
  /** Credential layer supplying it, such as `env` or `file`; absent while unconfigured. */
  readonly source?: string
  /**
   * Whether signing in or out can change the token. False while the launch
   * environment supplies it, or when no credential store is mounted.
   */
  readonly writable: boolean
}

/** Stored user preference; the shipped composition defaults it off. */
export interface SubagentModelSelectionSettings {
  /** Whether newly composed top-level Sessions receive model selection. */
  enabled: boolean
  /** Exact child LLM routes offered to newly composed top-level Sessions. */
  allowedModels: AllowedModelRoute[]
  /** Optional task router, read at each delegation. */
  router: SubagentRouterSettings
}

const DEFAULT_ROUTER: SubagentRouterSettings = {
  enabled: false, url: DEFAULT_ROUTER_URL, tokenEnv: 'ING_API_TOKEN', timeoutMs: 5000, hints: [],
}

const RouteHintSchema: z<SubagentRouteHint> = z.object({
  provider: z.string().min(1).required(),
  model: z.string().min(1).required(),
  sameAs: z.string().min(1),
  quality: z.union([z.const('low' as const), z.const('medium' as const), z.const('high' as const),
    z.const('frontier' as const)]),
  cost: z.union([z.const('free' as const), z.const('low' as const), z.const('medium' as const), z.const('high' as const)]),
})

const RouterSchema: z<SubagentRouterSettings> = z.object({
  enabled: z.boolean().default(false).description(
    'Beta. Delegations that name no model ask the router for one, sending it the task text and the allowed model names.'),
  url: z.string().default(DEFAULT_ROUTER.url),
  tokenEnv: z.string().default(DEFAULT_ROUTER.tokenEnv),
  timeoutMs: z.number().step(1).min(1).max(600_000).default(DEFAULT_ROUTER.timeoutMs),
  priority: z.union([z.const('quality' as const), z.const('cost' as const), z.const('speed' as const),
    z.const('balanced' as const)]),
  hints: z.array(RouteHintSchema).default([]),
})

/** Schema served to settings clients for the opt-in preference. */
export const SUBAGENT_MODEL_SELECTION_SETTINGS_SCHEMA: z<SubagentModelSelectionSettings> = z.object({
  enabled: z.boolean().default(false),
  allowedModels: z.array(AllowedModelRouteSchema).default([]),
  router: RouterSchema.default({ ...DEFAULT_ROUTER, hints: [] }),
})

/** Optional deployment base for the preference. */
export interface Config {
  /** Initial enabled state inherited when the user document does not override it. */
  enabled?: boolean
  /** Initial route list inherited when the user document does not override it. */
  allowedModels?: AllowedModelRoute[]
  /** Initial task router inherited when the user document does not override it. */
  router?: SubagentRouterSettings
}

/** Singleton settings owner read when delegation tools are composed for a Session. */
export class SubagentModelSelectionConfig extends Service {
  static Config: z<Config> = z.object({
    enabled: z.boolean().default(false),
    allowedModels: z.array(AllowedModelRouteSchema).default([]),
    router: RouterSchema.default({ ...DEFAULT_ROUTER, hints: [] }),
  })

  private source: () => SubagentModelSelectionSettings

  constructor(ctx: Context, config: Config = {}) {
    super(ctx, 'subagentModelSelection')
    // Cordis supplies the schema default; the fallback also covers direct construction.
    /* v8 ignore next */
    const entry: SubagentModelSelectionSettings = {
      enabled: config.enabled ?? false,
      allowedModels: config.allowedModels ?? [],
      router: { ...DEFAULT_ROUTER, ...config.router },
    }
    this.validate(entry)
    this.source = () => entry
    ctx.inject(['settings'], (settingsCtx) => {
      settingsCtx.settings.installSection(
        ctx,
        SUBAGENT_MODEL_SELECTION_SETTINGS_NAMESPACE,
        SUBAGENT_MODEL_SELECTION_SETTINGS_SCHEMA,
        entry,
        {
          setSource: (source) => { this.source = source },
          validate: (value) => { this.validate(value) },
          // Consumers snapshot per Session, so a settings update never rebuilds
          // the tool definitions of a Session that is already running.
          onChange: () => {},
        },
      )
    })
  }

  /**
   * Read a detached selection preference for the next eligible Session composition.
   * @returns the enabled state and exact allowed routes.
   */
  current(): Pick<SubagentModelSelectionSettings, 'enabled' | 'allowedModels'> {
    const current = this.source()
    return {
      enabled: current.enabled,
      allowedModels: current.allowedModels.map(route => ({ ...route })),
    }
  }

  /**
   * Read the task router for the delegation being made now.
   * @returns the router settings, or undefined while routing is off.
   */
  router(): SubagentRouterSettings | undefined {
    const router = this.routerSettings()
    return router.enabled && router.url.length > 0 ? router : undefined
  }

  /**
   * Ask the router how it recognizes each model new Sessions may use, sending
   * exactly what a routed delegation sends about them, so the user can see
   * which models need a hint. It asks even while routing is off.
   * @param llm - Live LLM runtime that describes each route.
   * @param signal - The caller's lifetime.
   * @returns the router's view of each allowed model, in their order.
   */
  describeRoutes(llm: Pick<LlmRuntime, 'resolveModelInfo'> | undefined, signal: AbortSignal): Promise<readonly RouterRouteView[]> {
    const router = this.routerSettings()
    if (router.url.length === 0) return Promise.reject(new Error('no router URL is set'))
    return this.routerToken().then(token => describeRoutes(router, this.current().allowedModels, llm, signal, token))
  }

  /**
   * The router's bearer token, resolved afresh for each request so a sign-in
   * or sign-out reaches the next delegation.
   * @returns the token, or undefined when none is stored or exported.
   */
  async routerToken(): Promise<string | undefined> {
    const name = this.routerSettings().tokenEnv
    const credentials = this.ctx.get('credentials')
    if (credentials === undefined) {
      // Without the credential seam the environment is the only place a token can be.
      const value = process.env[name]
      return value === undefined || value.length === 0 ? undefined : value
    }
    return (await credentials.resolve(credentialRef(name)))?.value
  }

  /**
   * Whether a router token is present and where it comes from, without the token.
   * @returns the token's reference, presence, source, and whether sign-in can replace it.
   */
  async routerTokenStatus(): Promise<RouterTokenStatus> {
    const tokenEnv = this.routerSettings().tokenEnv
    const credentials = this.ctx.get('credentials')
    if (credentials === undefined) {
      const value = process.env[tokenEnv]
      const configured = value !== undefined && value.length > 0
      return { tokenEnv, configured, ...configured ? { source: 'env' } : {}, writable: false }
    }
    const info = await credentials.describe(credentialRef(tokenEnv))
    return { tokenEnv, configured: info.configured, ...info.source === undefined ? {} : { source: info.source }, writable: info.writable }
  }

  /**
   * The router account the current token belongs to.
   * @param signal - The caller's lifetime.
   * @returns the account's address, or undefined without a token or when the router does not know it as an account's.
   */
  async routerAccount(signal: AbortSignal): Promise<string | undefined> {
    const token = await this.routerToken()
    return token === undefined ? undefined : routerAccount(this.routerSettings(), token, signal)
  }

  /**
   * Ask the router to email a one-time sign-in code. The first sign-in with
   * an address registers it. Refused before anything is sent when a token
   * from the environment would shadow the one signing in stores.
   * @param email - The address to sign in with.
   * @param signal - The caller's lifetime.
   */
  async requestSignInCode(email: string, signal: AbortSignal): Promise<void> {
    await this.writableCredentials()
    await requestSignInCode(this.routerSettings(), email, signal)
  }

  /**
   * Trade an emailed code for a router token and store it, so routed
   * delegations and the calibration list send it from the next request on.
   * @param email - The address the code was sent to.
   * @param code - The code as the user typed it.
   * @param signal - The caller's lifetime.
   * @returns the signed-in account's address.
   */
  async signIn(email: string, code: string, signal: AbortSignal): Promise<string> {
    const credentials = await this.writableCredentials()
    const signed = await redeemSignInCode(this.routerSettings(), email, code, signal)
    await credentials.set(credentialRef(this.routerSettings().tokenEnv), signed.token)
    return signed.email
  }

  /**
   * Revoke the stored router token at the router and remove it. The token is
   * removed even when the router cannot be reached, so signing out always
   * stops this machine from sending it.
   * @param signal - The caller's lifetime.
   */
  async signOut(signal: AbortSignal): Promise<void> {
    const credentials = await this.writableCredentials()
    const token = await this.routerToken()
    if (token !== undefined) {
      await revokeRouterToken(this.routerSettings(), token, signal).catch((error: unknown) => {
        signal.throwIfAborted()
        this.ctx.logger.warn(`router sign-out could not revoke the token: ${String(error)}`)
      })
    }
    await credentials.unset(credentialRef(this.routerSettings().tokenEnv))
  }

  /** The credential store, once it can change the router token; signing in or out is refused otherwise. */
  private async writableCredentials(): Promise<CredentialProvider> {
    const status = await this.routerTokenStatus()
    const credentials = this.ctx.get('credentials')
    if (credentials === undefined) throw new Error(`no credential store is mounted; set ${status.tokenEnv} instead`)
    if (!status.writable) throw new Error(`${status.tokenEnv} is set in the environment; unset it to sign in`)
    return credentials
  }

  /** The stored router with defaults filled; documents written before the router existed carry no `router` key. */
  private routerSettings(): SubagentRouterSettings {
    const stored = this.source().router
    return { ...DEFAULT_ROUTER, ...stored, hints: (stored?.hints ?? []).map(hint => ({ ...hint })) }
  }

  private validate(value: SubagentModelSelectionSettings): void {
    const url = value.router?.url ?? DEFAULT_ROUTER.url
    if (value.router?.tokenEnv !== undefined && !isCredentialRefName(value.router.tokenEnv)) {
      throw new Error('subagent router `tokenEnv` must be an environment variable name')
    }
    if (url.length > 0 && !/^https?:\/\/[^/]/.test(url)) {
      throw new Error('subagent router `url` must be an http or https URL')
    }
    if (value.router?.enabled === true && url.length === 0) {
      throw new Error('an enabled subagent router requires a `url`')
    }
    assertAllowedModelRoutes(value.allowedModels)
    if (value.enabled && value.allowedModels.length === 0) {
      throw new Error('enabled subagent model selection requires at least one allowed model')
    }
  }
}

export const name = 'subagent-model-selection-settings'
export default SubagentModelSelectionConfig
