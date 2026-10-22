/**
 * Event-only filesystem observation policy; it registers no service. A weak owner/target map
 * records every authoritative presence/absence observation, single-slot intent listeners derive
 * guards from that state, and the provider performs the atomic freshness/no-clobber check. Edits are
 * content-anchored by default: a unique exact match is the precondition, so an unread or changed file
 * need not be re-read; `editGuard: version` restores the read-before-edit refusal. Without
 * this plugin, tools retain the bare provider's unconditional mutation behavior. See the package
 * README for composition rules.
 * @module bake-fs-observation-policy
 */

import type { Context } from '@deepseek-ai/cordis'
import z from '@deepseek-ai/schemastery'
import { FsError } from 'bake-fs'
import type { FsEditIntent, FsObservation, FsTarget, FsWriteIntent } from 'bake-fs'
import type { FsObservationActor } from './types.ts'

export type { FsObservationActor } from './types.ts'

/**
 * Per-context observed-file state and the three `fs/*` decisions over it. One
 * instance is created per `apply()` so disposal can drop all state for HMR.
 */
class ObservedStateGate {
  constructor(private readonly editGuard: EditGuard) {}

  /**
   * Observed-file state, keyed first by the owner object (weakly held, so a
   * collected session frees its state), then by {@link FsTarget.targetKey}. An
   * entry's presence is the prior-observation record; its discriminant keeps
   * confirmed absence distinct from an unseen target.
   */
  private observed = new WeakMap<object, Map<string, FsObservation>>()

  /**
   * Derive the observed-state owner from the opaque event actor — normally the
   * active agent session. `undefined` when no owner can be derived (e.g. a
   * direct tool call with no agent); such calls read freely but cannot satisfy
   * the write/edit prior-observation policy.
   */
  private owner(actor: object | undefined): object | undefined {
    // tsgolint treats object as assignable to weak FsObservationActor, while tsc still requires the structural cast for property access.
    // See the analyzer-divergence consequence in .agents/notes/archived/process/2026-07-29-oxlint-linter.md.
    // oxlint-disable-next-line typescript/no-unnecessary-type-assertion -- The analyzers disagree on this weak type.
    return (actor as FsObservationActor | undefined)?.agent?.session
  }

  private get(owner: object, targetKey: string): FsObservation | undefined {
    return this.observed.get(owner)?.get(targetKey)
  }

  private set(owner: object, targetKey: string, observation: FsObservation): void {
    let byTarget = this.observed.get(owner)
    if (!byTarget) {
      byTarget = new Map()
      this.observed.set(owner, byTarget)
    }
    byTarget.set(targetKey, observation)
  }

  /** Drop all recorded state (HMR safety / disposal). */
  clear(): void {
    this.observed = new WeakMap()
  }

  /**
   * Decide the write intent: unseen or confirmed absent ⇒ `createIfAbsent`;
   * confirmed present ⇒ `replaceIfVersion` at the observed version.
   */
  writeIntent(target: FsTarget, actor: object | undefined): FsWriteIntent {
    const owner = this.owner(actor)
    const prior = owner ? this.get(owner, target.targetKey) : undefined
    return prior?.kind === 'present'
      ? { kind: 'replaceIfVersion', version: prior.version }
      : { kind: 'createIfAbsent' }
  }

  /**
   * Decide the edit guard. `anchored` passes the observed version, when there
   * is one, so the provider can report the basis and still guard `replaceAll`.
   * `version` rejects an unseen target with `FS_NOT_OBSERVED` and confirmed
   * absence with `FS_NOT_FOUND`, and supplies the observed version as the CAS basis.
   */
  editIntent(target: FsTarget, actor: object | undefined): FsEditIntent {
    const owner = this.owner(actor)
    const prior = owner ? this.get(owner, target.targetKey) : undefined
    if (this.editGuard === 'anchored') {
      return prior?.kind === 'present' ? { kind: 'anchored', version: prior.version } : { kind: 'anchored' }
    }
    if (!owner || prior === undefined) {
      throw new FsError(`edit requires reading "${target.displayPath}" first`, 'FS_NOT_OBSERVED')
    }
    if (prior.kind === 'absent') {
      throw new FsError(`cannot edit "${target.displayPath}": not found`, 'FS_NOT_FOUND')
    }
    return { version: prior.version }
  }

  /** Record an authoritative present or absent observation for this owner and target. */
  observe(target: FsTarget, observation: FsObservation, actor: object | undefined): void {
    const owner = this.owner(actor)
    if (owner) this.set(owner, target.targetKey, observation)
  }
}

/** How the policy guards `edit`; `write` is always version-guarded. */
export type EditGuard = 'anchored' | 'version'

/** Cordis plugin name used by loader diagnostics. */
export const name = 'fs-observation-policy'

/** Plugin config (all optional — `Config` supplies the defaults). */
export interface Config {
  /**
   * `anchored` (default) lets a unique exact match stand in for a fresh read;
   * `version` refuses an edit of an unread or changed file.
   */
  editGuard?: EditGuard
}

export const Config: z<Config> = z.object({
  editGuard: z.union(['anchored', 'version']).default('anchored'),
})

/**
 * Register the three `fs/*` listeners. No `inject` — this plugin reads no
 * services; it operates only on its own `WeakMap`. The waterfalls are unbound
 * (the tool dispatches them with no `this`), so the listeners take the raw
 * `(target, actor, next)` arguments.
 */
export function apply(ctx: Context, config: Config = {}): void {
  const gate = new ObservedStateGate(config.editGuard ?? 'anchored')

  ctx.effect(() => () => {
    // Drop all recorded state on disposal so a reloaded plugin starts clean
    // (HMR safety). The WeakMap itself would be GC'd, but replacing it makes the
    // release observable and immediate for tests.
    gate.clear()
  }, 'fs-observation-policy observed-state teardown')

  // fs/write-intent: occupy the single decision slot — do NOT call next().
  // Deferred through Promise.resolve().then so the declared Promise return type
  // holds (a throw rejects, never escapes synchronously through the waterfall).
  ctx.on('fs/write-intent', (target, actor) => Promise.resolve().then(() => gate.writeIntent(target, actor)))

  // fs/edit-intent: occupy the single decision slot — do not call next().
  ctx.on('fs/edit-intent', (target, actor) => Promise.resolve().then(() => gate.editIntent(target, actor)))

  // fs/observed must remain synchronous and non-throwing: emit does not await
  // promises, and successful mutations have already committed. WeakMap.set
  // satisfies that contract for both presence and absence.
  ctx.on('fs/observed', (target, observation, actor) => {
    gate.observe(target, observation, actor)
  })
}
