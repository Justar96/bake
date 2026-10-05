/**
 * Signing in to providers, and out again. Two kinds of credential reach one surface.
 *
 * A key reference is a provider that reads a named key, such as
 * `DEEPSEEK_API_KEY`. The value is stored once through `ctx.credentials`.
 * An authorization flow is a provider whose credential can only be obtained
 * by talking to a human. The provider registers a flow, and any surface can
 * run an attempt and render the notices and prompts it produces.
 *
 * This module knows about neither provider. It lists what the composition
 * offers and drives whichever kind the user picked, so a provider added by a
 * patch appears here without a code change.
 *
 * @module @deepseek-ai/dsh-tui-app/login
 */

import type { Context } from '@deepseek-ai/cordis'
import type { ModelSelection } from 'bake-agent'
import { credentialKeyId, credentialKeyScope, credentialRef, type CredentialKey } from '@deepseek-ai/dsh-credentials'
import { normalizeApiKey } from '@deepseek-ai/dsh-llm'
import type { AuthorizationNotice, AuthorizationPrompt } from '@deepseek-ai/dsh-authorization/types'
import type {} from '@deepseek-ai/dsh-authorization'
import { suggestCommand } from '@dsh-tui/ui/completion.ts'
import type { LoginField } from '@dsh-tui/ui/interaction.tsx'
import { CLIPROXYAPI_ID, CLIPROXYAPI_KEY, cliProxyEndpoints, configureCliProxyApi, type CliProxySetupCopy } from './cliproxyapi.ts'
import type { TuiCopy } from '@dsh-tui/ui/copy.ts'

/**
 * One key reference the composition offers, as its profile names it: the
 * reference alone, or with the provider name people know it by, the route
 * the key unlocks, and the model a first sign-in starts that route on.
 */
export type CredentialTargetConfig = string
  | { readonly ref: string; readonly label?: string; readonly provider?: string; readonly model?: string }

/** One authorization flow the composition offers: its credential key, and the model a first sign-in starts on. */
export type SignInFlowConfig = string | { readonly key: string; readonly model?: string }

/** What the composition offers `/login`: key references, and which authorization flows. */
export interface LoginSources {
  /** Key references this profile's providers read, in the order `/login` lists them. */
  readonly refs: readonly CredentialTargetConfig[]
  /** Authorization flows to offer, in order; absent offers every registered flow. */
  readonly flows?: readonly SignInFlowConfig[]
}

/**
 * The settings namespace and record scope of the pi-ai adapter. A flow whose
 * record lives under it signs a catalog provider in, and that provider's route
 * in this namespace is what serves the models.
 */
export const PI_AI_NAMESPACE = 'llm-pi-ai'

/** One thing the user can sign in to, whichever seam provides it. */
export interface LoginTarget {
  /** The seam's own address: a key reference, `cliproxyapi`, or a flow's credential key. */
  readonly id: string
  /** What `/login` and `/logout` offer the user to type: short and lowercase. */
  readonly name: string
  /** User-facing name. */
  readonly label: string
  /** Where it lives: the key reference, or the proxy's host once one is set up. */
  readonly detail?: string
  /** Whether the credential is already usable. */
  readonly configured: boolean
  /** Whether this surface can change it, or a read-only source shadows it. */
  readonly writable: boolean
  /** The credential layer supplying a configured key, such as `env` or `file`. */
  readonly source?: string
  /** The provider route this credential serves, when the composition says. */
  readonly provider?: string
  /** The model a first sign-in starts that route on; absent, the first it lists. */
  readonly model?: string
  /** A flow's ways of signing in, most preferred first. */
  readonly methods?: readonly { readonly id: string; readonly label: string }[]
  /** Which seam backs it. */
  readonly kind: 'key' | 'flow' | 'cliproxyapi'
}

/**
 * One question during a sign-in: an authorization prompt, plus what this
 * surface can show about the field's place and the last answer's refusal.
 */
export type LoginPrompt = AuthorizationPrompt & Omit<LoginField, 'message' | 'secret' | 'placeholder'>

/** How the surface asks the human for something during a login. */
export interface LoginInteraction {
  /** Show one-way progress a flow reports, such as a page to open; never carries a secret. */
  notify: (notice: AuthorizationNotice) => void
  /**
   * Show, or clear, this surface's own passing status, such as a check in
   * flight. Unlike a flow's notice, it is gone once the next field opens.
   */
  progress?: (text: string | undefined) => void
  /** Ask a question and resolve with the answer, or reject to decline. */
  prompt: (prompt: LoginPrompt) => Promise<string>
}

/** The reference name of one configured key target. */
export const refOf = (target: CredentialTargetConfig): string => typeof target === 'string' ? target : target.ref

/**
 * The name a label is typed as: lowercase letters and digits, words joined by `-`.
 * @param label - the user-facing name.
 * @returns the typed name, or empty when the label has no letters or digits.
 */
function typedName(label: string): string {
  return label.toLowerCase().replace(/[^a-z0-9]+/gu, '-').replace(/^-+|-+$/gu, '')
}

/**
 * List everything this composition can sign in to.
 *
 * Key references come from configuration, because only the composition knows
 * which providers it mounted. Flows come from the authorization registry,
 * which already knows its own.
 *
 * @param ctx - the settled plugin context.
 * @param sources - the key references and flows this profile offers.
 * @returns the targets: key references, CLIProxyAPI, then flows, each in configuration order.
 */
export async function listTargets(ctx: Context, sources: LoginSources): Promise<readonly LoginTarget[]> {
  const targets: LoginTarget[] = []
  const taken = new Set<string>()
  // A label's typed name goes to the first target that claims it; a later
  // one keeps its own address, so no two targets answer to one name.
  const claim = (preferred: string, fallback: string): string => {
    const name = preferred !== '' && !taken.has(preferred) ? preferred : fallback
    taken.add(name)
    return name
  }
  const credentials = ctx.get('credentials')
  if (credentials !== undefined) {
    for (const entry of sources.refs) {
      const ref = refOf(entry)
      const label = typeof entry === 'string' ? undefined : entry.label
      const provider = typeof entry === 'string' ? undefined : entry.provider
      const model = typeof entry === 'string' ? undefined : entry.model
      // `describe` reports whether a value exists and whether it can be
      // changed. It never returns the value itself.
      const info = await credentials.describe(credentialRef(ref))
      targets.push({ id: ref, name: claim(label === undefined ? '' : typedName(label), ref), label: label ?? ref,
        ...label === undefined ? {} : { detail: ref },
        configured: info.configured, writable: info.writable,
        ...info.source === undefined ? {} : { source: info.source },
        ...provider === undefined ? {} : { provider }, ...model === undefined ? {} : { model }, kind: 'key' })
    }
    const info = await credentials.describe(credentialRef(CLIPROXYAPI_KEY))
    const configured = info.configured && (ctx.get('llm')?.listProviders().some(provider => provider.id === CLIPROXYAPI_ID) ?? false)
    const host = configured ? proxyHost(ctx) : undefined
    targets.push({ id: CLIPROXYAPI_ID, name: claim(CLIPROXYAPI_ID, CLIPROXYAPI_ID), label: 'CLIProxyAPI',
      ...host === undefined ? {} : { detail: host },
      configured, writable: info.writable && (ctx.get('settings')?.writable ?? false),
      ...info.source === undefined ? {} : { source: info.source }, provider: CLIPROXYAPI_ID, kind: 'cliproxyapi' })
  }
  const registered = ctx.get('authorization')?.list() ?? []
  const offered = sources.flows === undefined ? registered.map(entry => ({ entry, model: undefined }))
    : sources.flows.flatMap(flow => {
      const key = typeof flow === 'string' ? flow : flow.key
      const model = typeof flow === 'string' ? undefined : flow.model
      return registered.filter(entry => entry.key === key).map(entry => ({ entry, model }))
    })
  for (const { entry, model } of offered) {
    // A stored grant is the flow's credential; an attempt already running is
    // the one state that stops this surface from starting another.
    const record = await credentials?.describeRecord(entry.key).catch(() => undefined)
    const piAi = credentialKeyScope(entry.key) === PI_AI_NAMESPACE
    targets.push({
      id: entry.key, name: claim(typedName(credentialKeyId(entry.key)), entry.key), label: entry.label,
      detail: entry.methods.map(method => method.label).join(' \u00b7 '),
      configured: record?.configured ?? false,
      writable: !entry.inFlight,
      ...piAi ? { provider: credentialKeyId(entry.key) } : {},
      ...model === undefined ? {} : { model },
      methods: entry.methods.map(method => ({ id: method.id, label: method.label })),
      kind: 'flow',
    })
  }
  return targets
}

/** The host of the saved CLIProxyAPI route, for naming where a configured proxy lives. */
function proxyHost(ctx: Context): string | undefined {
  const section = ctx.get('settings')?.get('llm-pi-ai') as { providers?: Record<string, { baseURL?: unknown }> } | undefined
  const baseURL = section?.providers?.[CLIPROXYAPI_ID]?.baseURL
  if (typeof baseURL !== 'string') return undefined
  try { return new URL(cliProxyEndpoints(baseURL).root).host } catch { return undefined }
}

/** Which target a typed name meant, or the nearest one when it meant none. */
export type TargetMatch =
  | { readonly kind: 'found', readonly target: LoginTarget }
  | { readonly kind: 'unknown', readonly suggestion?: string }

/**
 * Find the target a typed name means: its name, address, or label, in any case.
 * @param targets - the targets on offer.
 * @param typed - what the user typed after the command.
 * @returns the target, or the closest name when none matches.
 */
export function findTarget(targets: readonly LoginTarget[], typed: string): TargetMatch {
  const wanted = typed.trim().toLowerCase()
  const target = targets.find(candidate => candidate.name === wanted)
    ?? targets.find(candidate => candidate.id.toLowerCase() === wanted || candidate.label.toLowerCase() === wanted)
  if (target !== undefined) return { kind: 'found', target }
  const suggestion = suggestCommand(targets.map(candidate => candidate.name), wanted)
  return suggestion === undefined ? { kind: 'unknown' } : { kind: 'unknown', suggestion }
}

/**
 * The model a fresh session starts on.
 *
 * A saved selection whose provider is signed in stays. Otherwise the session
 * starts on the first model of the first signed-in target that names its
 * provider, in `/login` order, so it opens on a provider that can answer.
 * With nothing signed in it starts on no model at all; a saved selection the
 * composition cannot vouch for, because no target names its provider, is
 * kept as it is.
 * @param ctx - the settled plugin context.
 * @param sources - the key references and flows this profile offers.
 * @param selection - the saved default selection, if any.
 * @returns the selection to start on, or undefined when none can answer.
 */
export async function availableSelection(ctx: Context, sources: LoginSources,
  selection: ModelSelection | undefined): Promise<ModelSelection | undefined> {
  const targets = await listTargets(ctx, sources)
  const serving = (provider: string): readonly LoginTarget[] => targets.filter(target => target.provider === provider)
  if (selection !== undefined) {
    const owners = serving(selection.provider)
    if (owners.length === 0 || owners.some(target => target.configured)) return selection
  }
  const llm = ctx.get('llm')
  for (const target of targets) {
    if (!target.configured || target.provider === undefined) continue
    // A catalog that cannot be read passes to the next signed-in provider.
    const model = startingModel(target, await llm?.listModels(target.provider).catch(() => undefined) ?? [])
    if (model !== undefined) return { provider: target.provider, model }
  }
  return selection !== undefined && serving(selection.provider).length === 0 ? selection : undefined
}

/**
 * Whether a signed-in provider has a route to serve it: one already stood,
 * the sign-in added one, or settings could not take one.
 */
export type RouteState = 'present' | 'added' | 'unwritable'

/**
 * The model a signed-in target starts its route on: the one the profile
 * names when the route lists it, otherwise the first it lists.
 * @param target - the signed-in target.
 * @param listed - the route's models, in its order.
 * @returns the model id, or undefined when the route lists none.
 */
export function startingModel(target: Pick<LoginTarget, 'model'>, listed: readonly { readonly id: string }[]): string | undefined {
  return listed.find(entry => entry.id === target.model)?.id ?? listed[0]?.id
}

/** What one `/login` attempt did. */
export type LoginResult =
  | { readonly kind: 'stored', readonly target: LoginTarget, readonly models?: number, readonly route?: RouteState }
  | { readonly kind: 'cancelled', readonly target: LoginTarget }
  /** A source this surface cannot write, such as the environment, supplies the key. */
  | { readonly kind: 'read-only', readonly target: LoginTarget }
  | { readonly kind: 'unknown-target', readonly id: string, readonly suggestion?: string }

/** The labels a sign-in shows. */
export type LoginCopy = CliProxySetupCopy & Pick<TuiCopy, 'apiKey' | 'keyStoredLocally' | 'chooseMethod'>

/**
 * Give a signed-in pi-ai catalog provider a route, so its installed models
 * reach `/model`. An empty profile is the whole route: the catalog supplies
 * the endpoint and models, and the stored sign-in authenticates it.
 * @param ctx - the settled plugin context.
 * @param provider - the catalog provider id, which is also its route key.
 * @returns whether a route stood, was added, or could not be.
 */
async function ensureRoute(ctx: Context, provider: string): Promise<RouteState> {
  const settings = ctx.get('settings')
  const section = settings?.get(PI_AI_NAMESPACE) as { providers?: Readonly<Record<string, unknown>> } | undefined
  if (section?.providers?.[provider] !== undefined) return 'present'
  if (settings === undefined || !settings.writable) return 'unwritable'
  await settings.mutate(PI_AI_NAMESPACE, [{ op: 'set', path: ['providers', provider], value: {} }])
  return 'added'
}

/**
 * Sign in to one target.
 *
 * A key reference is prompted as a secret and stored. A flow runs through the
 * authorization seam, which reports `authorized` only after the record is
 * committed. A success here always means the credential is stored. A refused
 * answer asks again in the same panel with the reason, so a typo costs one
 * field instead of the whole sign-in.
 *
 * @param ctx - the settled plugin context.
 * @param targets - the targets listed for this session.
 * @param id - the target the user named: its name, address, or label.
 * @param interaction - how to reach the human.
 * @param signal - command cancellation lifetime.
 * @param copy - localized sign-in labels.
 * @returns what happened, for the surface to report.
 */
export async function login(
  ctx: Context,
  targets: readonly LoginTarget[],
  id: string,
  interaction: LoginInteraction,
  signal: AbortSignal,
  copy: LoginCopy,
): Promise<LoginResult> {
  signal.throwIfAborted()
  const match = findTarget(targets, id)
  if (match.kind === 'unknown') return { kind: 'unknown-target', id, ...match.suggestion === undefined ? {} : { suggestion: match.suggestion } }
  const { target } = match
  const title = `${copy.signInTitle} \u00b7 ${target.label}`

  if (target.kind === 'cliproxyapi') {
    let declined = false
    let models: number
    try {
      models = await configureCliProxyApi(ctx, async question => {
        // A check that just failed says so in the field that asks again.
        interaction.progress?.(undefined)
        try { return await interaction.prompt(question) }
        catch (error) { declined = true; throw error }
      }, signal, copy, fetch, host => interaction.progress?.(`${copy.cliProxyChecking} ${host}\u2026`))
    } catch (error) {
      if (declined) return { kind: 'cancelled', target }
      throw error
    }
    return { kind: 'stored', target, models }
  }

  if (target.kind === 'flow') {
    const authorization = ctx.get('authorization')
    if (authorization === undefined) return { kind: 'cancelled', target }
    // A subscription sign-in and a pasted key are both offered; the flow's own
    // order puts its preference first.
    let method = target.methods?.[0]?.id
    if ((target.methods?.length ?? 0) > 1) {
      try {
        method = await interaction.prompt({ kind: 'select', title, message: copy.chooseMethod,
          options: target.methods!.map(option => ({ id: option.id, label: option.label })) })
      } catch (declined) {
        void declined
        return { kind: 'cancelled', target }
      }
      signal.throwIfAborted()
    }
    // A flow target's id was taken directly from `authorization.list()`.
    const outcome = await authorization.begin({ key: target.id as CredentialKey, signal,
      ...method === undefined ? {} : { method },
      interaction: { notify: interaction.notify, prompt: question => interaction.prompt({ ...question, title }) } })
    if (outcome.status !== 'authorized') return { kind: 'cancelled', target }
    if (target.provider === undefined || credentialKeyScope(target.id as CredentialKey) !== PI_AI_NAMESPACE) return { kind: 'stored', target }
    return { kind: 'stored', target, route: await ensureRoute(ctx, target.provider) }
  }

  const credentials = ctx.get('credentials')
  if (credentials === undefined) return { kind: 'cancelled', target }
  if (!target.writable) return { kind: 'read-only', target }
  let error: string | undefined
  for (;;) {
    let value: string
    try {
      value = await interaction.prompt({ kind: 'secret', title, message: copy.apiKey,
        hint: `${target.id} \u00b7 ${copy.keyStoredLocally}`, ...error === undefined ? {} : { error } })
    } catch (declined) {
      // A rejected prompt is a declined authorization attempt.
      void declined
      return { kind: 'cancelled', target }
    }
    signal.throwIfAborted()
    const checked = normalizeApiKey(value)
    if (!checked.ok) { error = checked.reason === 'empty' ? copy.loginEmpty : copy.keyInvalid; continue }
    await credentials.set(credentialRef(target.id), checked.value)
    return { kind: 'stored', target }
  }
}

/** What one `/logout` did. */
export type LogoutResult =
  | { readonly kind: 'removed', readonly target: LoginTarget }
  | { readonly kind: 'read-only', readonly target: LoginTarget }
  | { readonly kind: 'not-configured', readonly target: LoginTarget }
  | { readonly kind: 'unknown-target', readonly id: string, readonly suggestion?: string }

/**
 * Remove what a sign-in stored for one target.
 *
 * A key is unset from the managed store. CLIProxyAPI loses its settings
 * route first, which unregisters its models, then a stored key. A flow's
 * grant record is deleted. A key another source still supplies after the
 * unset, such as the environment or a `.env` file, is reported instead of
 * claimed as removed: nothing was signed out while it keeps resolving.
 *
 * @param ctx - the settled plugin context.
 * @param targets - the targets listed for this session.
 * @param id - the target the user named: its name, address, or label.
 * @returns what happened, for the surface to report.
 */
export async function logout(ctx: Context, targets: readonly LoginTarget[], id: string): Promise<LogoutResult> {
  const match = findTarget(targets, id)
  if (match.kind === 'unknown') return { kind: 'unknown-target', id, ...match.suggestion === undefined ? {} : { suggestion: match.suggestion } }
  const { target } = match
  if (!target.configured) return { kind: 'not-configured', target }
  const credentials = ctx.get('credentials')
  if (credentials === undefined) return { kind: 'read-only', target }
  if (target.kind === 'flow') {
    if (!target.writable) return { kind: 'read-only', target }
    await credentials.deleteRecord(target.id as CredentialKey)
    // The empty route a sign-in added goes with it; a route someone shaped stays theirs.
    const settings = ctx.get('settings')
    if (target.provider !== undefined && settings?.writable === true
      && credentialKeyScope(target.id as CredentialKey) === PI_AI_NAMESPACE) {
      const section = settings.get(PI_AI_NAMESPACE) as { providers?: Readonly<Record<string, unknown>> } | undefined
      const route = section?.providers?.[target.provider]
      if (route !== null && typeof route === 'object' && Object.keys(route).length === 0) {
        await settings.mutate(PI_AI_NAMESPACE, [{ op: 'unset', path: ['providers', target.provider] }])
      }
    }
    return { kind: 'removed', target }
  }
  if (target.kind === 'cliproxyapi') {
    const settings = ctx.get('settings')
    if (settings === undefined || !settings.writable) return { kind: 'read-only', target }
    await settings.mutate('llm-pi-ai', [{ op: 'unset', path: ['providers', CLIPROXYAPI_ID] }])
    // Without its route the key reaches no provider; an inherited one is left for its owner.
    const ref = credentialRef(CLIPROXYAPI_KEY)
    if ((await credentials.describe(ref)).writable) await credentials.unset(ref)
    return { kind: 'removed', target }
  }
  if (!target.writable) return { kind: 'read-only', target }
  const ref = credentialRef(target.id)
  await credentials.unset(ref)
  return (await credentials.describe(ref)).configured ? { kind: 'read-only', target } : { kind: 'removed', target }
}
