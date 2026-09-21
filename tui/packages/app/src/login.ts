/**
 * Signing in to providers. Two kinds of credential reach one surface:
 *
 * - **Key references** — a provider that reads a named key (`DEEPSEEK_API_KEY`)
 *   needs the value stored once through `ctx.credentials`.
 * - **Authorization flows** — a provider whose credential can only be obtained
 *   by talking to a human registers a flow, and any surface can run an attempt
 *   and render the notices and prompts it produces.
 *
 * This module knows about neither provider. It lists what the composition
 * offers and drives whichever kind the user picked, so a provider added by a
 * patch appears here without a code change.
 *
 * @module @deepseek-ai/dsh-tui-app/login
 */

import type { Context } from '@deepseek-ai/cordis'
import { credentialRef } from '@deepseek-ai/dsh-credentials'
import type { AuthorizationNotice, AuthorizationPrompt } from '@deepseek-ai/dsh-authorization/types'

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
  readonly kind: 'key' | 'flow'
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
 * Key references come from configuration because only the composition knows
 * which providers it mounted; flows come from the authorization registry,
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
      // changed; it never returns the value itself.
      const info = await credentials.describe(credentialRef(ref))
      targets.push({ id: ref, label: ref, configured: info.configured, writable: info.writable, kind: 'key' })
    }
  }
  for (const entry of ctx.get('authorization')?.list() ?? []) {
    targets.push({
      id: entry.key,
      label: entry.label,
      // A flow's credential is known to the flow, not to this surface; an
      // attempt that is already running is the one state worth reporting.
      configured: false,
      writable: !entry.inFlight,
      kind: 'flow',
    })
  }
  return targets
}

/** What one `/login` attempt did. */
export type LoginResult =
  | { readonly kind: 'stored', readonly target: string }
  | { readonly kind: 'cancelled', readonly target: string }
  | { readonly kind: 'unknown-target', readonly id: string }

/**
 * Sign in to one target.
 *
 * A key reference is prompted as a secret and stored; a flow runs through the
 * authorization seam, which reports `authorized` only after the record is
 * committed, so a success here always means the credential is really stored.
 *
 * @param ctx - the settled plugin context.
 * @param targets - the targets listed for this session.
 * @param id - the target the user named.
 * @param interaction - how to reach the human.
 * @param signal - command cancellation lifetime.
 * @param promptLabel - localized label preceding the credential reference.
 * @returns what happened, for the surface to report.
 */
export async function login(
  ctx: Context,
  targets: readonly LoginTarget[],
  id: string,
  interaction: LoginInteraction,
  signal: AbortSignal,
  promptLabel: string,
): Promise<LoginResult> {
  signal.throwIfAborted()
  const target = targets.find(candidate => candidate.id === id)
  if (target === undefined) return { kind: 'unknown-target', id }

  if (target.kind === 'flow') {
    const authorization = ctx.get('authorization')
    if (authorization === undefined) return { kind: 'cancelled', target: id }
    const outcome = await authorization.begin({ key: target.id, interaction, signal })
    return outcome.status === 'authorized' ? { kind: 'stored', target: id } : { kind: 'cancelled', target: id }
  }

  const credentials = ctx.get('credentials')
  if (credentials === undefined) return { kind: 'cancelled', target: id }
  let value: string
  try {
    value = await interaction.prompt({ kind: 'secret', message: `${promptLabel}: ${target.label}` })
  } catch (error) {
    // Prompt rejection represents a declined authorization attempt.
    void error
    return { kind: 'cancelled', target: id }
  }
  signal.throwIfAborted()
  const trimmed = value.trim()
  if (trimmed === '') return { kind: 'cancelled', target: id }
  await credentials.set(credentialRef(target.id), trimmed)
  return { kind: 'stored', target: id }
}
