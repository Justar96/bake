/**
 * Signing in to providers. Two kinds of credential reach one surface.
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
import type { ModelSelection } from '@deepseek-ai/dsh-agent'
import { credentialRef, type CredentialKey } from '@deepseek-ai/dsh-credentials'
import type { AuthorizationNotice, AuthorizationPrompt } from '@deepseek-ai/dsh-authorization/types'
import { CLIPROXYAPI_ID, CLIPROXYAPI_KEY, configureCliProxyApi } from './cliproxyapi.ts'
import type { TuiCopy } from '@dsh-tui/ui/copy.ts'

/** One thing the user can sign in to, whichever seam provides it. */
export interface LoginTarget {
  /** What the user types after `/login`. */
  readonly id: string
  /** User-facing description. */
  readonly label: string
  /** Whether the credential is already usable. */
  readonly configured: boolean
  /** Whether this surface can change it, or a read-only source shadows it. */
  readonly writable: boolean
  /** Which seam backs it. */
  readonly kind: 'key' | 'flow' | 'cliproxyapi'
}

/** How the surface asks the human for something during a login. */
export interface LoginInteraction {
  /** Show one-way progress; never carries a secret. */
  notify: (notice: AuthorizationNotice) => void
  /** Ask a question and resolve with the answer, or reject to decline. */
  prompt: (prompt: AuthorizationPrompt) => Promise<string>
}

/**
 * List everything this composition can sign in to.
 *
 * Key references come from configuration, because only the composition knows
 * which providers it mounted. Flows come from the authorization registry,
 * which already knows its own.
 *
 * @param ctx - the settled plugin context.
 * @param refs - credential reference names this profile's providers read.
 * @returns the targets, key references first, in configuration order.
 */
export async function listTargets(ctx: Context, refs: readonly string[]): Promise<readonly LoginTarget[]> {
  const targets: LoginTarget[] = []
  const credentials = ctx.get('credentials')
  if (credentials !== undefined) {
    for (const ref of refs) {
      // `describe` reports whether a value exists and whether it can be
      // changed. It never returns the value itself.
      const info = await credentials.describe(credentialRef(ref))
      targets.push({ id: ref, label: ref, configured: info.configured, writable: info.writable, kind: 'key' })
    }
    const info = await credentials.describe(credentialRef(CLIPROXYAPI_KEY))
    targets.push({ id: CLIPROXYAPI_ID, label: 'CLIProxyAPI',
      configured: info.configured && (ctx.get('llm')?.listProviders().some(provider => provider.id === CLIPROXYAPI_ID) ?? false),
      writable: info.writable && (ctx.get('settings')?.writable ?? false), kind: 'cliproxyapi' })
  }
  for (const entry of ctx.get('authorization')?.list() ?? []) {
    targets.push({
      id: entry.key,
      label: entry.label,
      // A flow's credential is known to the flow, not to this surface. An
      // attempt that is already running is the only state worth reporting.
      configured: false,
      writable: !entry.inFlight,
      kind: 'flow',
    })
  }
  return targets
}

/**
 * The model a fresh session starts on. The profile's default provider reads
 * the listed key references; when none of them holds a value and CLIProxyAPI
 * is set up, the session starts on CLIProxyAPI's first model instead of a
 * provider that cannot answer.
 * @param ctx - the settled plugin context.
 * @param refs - credential reference names the default provider reads.
 * @param selection - the configured default selection.
 * @returns the configured selection, or the CLIProxyAPI fallback.
 */
export async function availableSelection(ctx: Context, refs: readonly string[], selection: ModelSelection): Promise<ModelSelection> {
  if (refs.length === 0 || selection.provider === CLIPROXYAPI_ID) return selection
  const targets = await listTargets(ctx, refs)
  if (targets.some(target => target.kind === 'key' && target.configured)) return selection
  if (!targets.some(target => target.kind === 'cliproxyapi' && target.configured)) return selection
  // A catalog that cannot be read leaves the configured default in place.
  const model = (await ctx.get('llm')?.listModels(CLIPROXYAPI_ID).catch(() => undefined))?.[0]
  return model === undefined ? selection : { provider: CLIPROXYAPI_ID, model: model.id }
}

/** What one `/login` attempt did. */
export type LoginResult =
  | { readonly kind: 'stored', readonly target: string, readonly models?: number }
  | { readonly kind: 'cancelled', readonly target: string }
  | { readonly kind: 'unknown-target', readonly id: string }

/**
 * Sign in to one target.
 *
 * A key reference is prompted as a secret and stored. A flow runs through the
 * authorization seam, which reports `authorized` only after the record is
 * committed. A success here always means the credential is stored.
 *
 * @param ctx - the settled plugin context.
 * @param targets - the targets listed for this session.
 * @param id - the target the user named.
 * @param interaction - how to reach the human.
 * @param signal - command cancellation lifetime.
 * @param copy - localized credential and CLIProxyAPI prompt labels.
 * @returns what happened, for the surface to report.
 */
export async function login(
  ctx: Context,
  targets: readonly LoginTarget[],
  id: string,
  interaction: LoginInteraction,
  signal: AbortSignal,
  copy: Pick<TuiCopy, 'pasteCredential' | 'cliProxyUrl' | 'cliProxyKey' | 'cliProxyChecking'>,
): Promise<LoginResult> {
  signal.throwIfAborted()
  const target = targets.find(candidate => candidate.id === id)
  if (target === undefined) return { kind: 'unknown-target', id }

  if (target.kind === 'cliproxyapi') {
    let declined = false
    let models: number
    try {
      models = await configureCliProxyApi(ctx, async question => {
        try {
          const answer = await interaction.prompt(question)
          if (question.kind === 'secret') interaction.notify({ message: copy.cliProxyChecking })
          return answer
        }
        catch (error) { declined = true; throw error }
      }, signal, { url: copy.cliProxyUrl, key: copy.cliProxyKey })
    } catch (error) {
      if (declined) return { kind: 'cancelled', target: id }
      throw error
    }
    return { kind: 'stored', target: id, models }
  }

  if (target.kind === 'flow') {
    const authorization = ctx.get('authorization')
    if (authorization === undefined) return { kind: 'cancelled', target: id }
    // A flow target's id was taken directly from `authorization.list()`.
    const outcome = await authorization.begin({ key: target.id as CredentialKey, interaction, signal })
    return outcome.status === 'authorized' ? { kind: 'stored', target: id } : { kind: 'cancelled', target: id }
  }

  const credentials = ctx.get('credentials')
  if (credentials === undefined) return { kind: 'cancelled', target: id }
  let value: string
  try {
    value = await interaction.prompt({ kind: 'secret', message: `${copy.pasteCredential}: ${target.label}` })
  } catch (error) {
    // A rejected prompt is a declined authorization attempt.
    void error
    return { kind: 'cancelled', target: id }
  }
  signal.throwIfAborted()
  const trimmed = value.trim()
  if (trimmed === '') return { kind: 'cancelled', target: id }
  await credentials.set(credentialRef(target.id), trimmed)
  return { kind: 'stored', target: id }
}
