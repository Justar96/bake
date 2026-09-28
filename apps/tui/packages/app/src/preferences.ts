/**
 * The terminal's user settings: the `tui` namespace of the settings document,
 * and the `/settings` panel that edits it beside the session defaults other
 * plugins register there: the default model (`agent-default-model`, which the
 * session's `/model` picker writes) and the default access preset.
 *
 * The profile's `tui-runner` config is the namespace's base layer, so a
 * user's choice overrides the profile and `--screen` overrides both. Without
 * a settings service the panel still changes the running process, and says
 * the change ends with it.
 *
 * @module @dsh-tui/app/preferences
 */
import type { Context } from '@deepseek-ai/cordis'
import z from '@deepseek-ai/schemastery'
import type { SettingsProvider, SettingsScope } from '@deepseek-ai/dsh-settings'
import type { CommandResult } from '@deepseek-ai/dsh-commands'
import type {} from '@deepseek-ai/dsh-agent-default-model'
import { PERMISSION_SETTINGS_NAMESPACE } from '@deepseek-ai/dsh-permission-presets'
import type { TuiCopy } from '@dsh-tui/ui/copy.ts'
import { compactPath } from '@dsh-tui/ui/present.ts'
import type { Interactions } from './interactions.ts'

/** The namespace this surface owns in the settings document. */
export const SETTINGS_NAMESPACE = 'tui'

/** What a user may change about the terminal surface. */
export interface TuiSettings {
  /** Read once at launch. */
  readonly screen: 'inline' | 'fullscreen'
  /** Read once at launch. */
  readonly locale: 'en' | 'zh'
  readonly composerFrame: 'round' | 'classic' | 'auto'
  /** Whether the header names the goal's objective after its state. */
  readonly goalObjective: boolean
  readonly resultLines: number
  readonly completionLimit: number
  readonly doubleInterruptMs: number
}

/** Schema of the `tui` namespace. Defaults match the runner's profile config. */
export const TuiSettings: z<TuiSettings> = z.object({
  screen: z.union(['inline', 'fullscreen']).default('inline'),
  locale: z.union(['en', 'zh']).default('en'),
  composerFrame: z.union(['round', 'classic', 'auto']).default('auto'),
  goalObjective: z.boolean().default(false),
  resultLines: z.number().min(0).step(1).default(4),
  completionLimit: z.number().min(1).step(1).default(8),
  doubleInterruptMs: z.number().min(1).default(2000),
})

/** One value a row offers. */
interface Option {
  readonly value: string
  readonly label: string
  /** Secondary text in the value picker. */
  readonly description?: string
}

/** One row of the panel, with the values it offers. */
interface Row {
  readonly key: string
  readonly label: string
  readonly current: string
  readonly options: readonly Option[]
  /** What the row shows for its value; absent, the current option's label. */
  readonly shown?: string
  /** When the change takes effect, when that is not now. */
  readonly status?: string
  /** Two options, so Enter flips it instead of opening a list. */
  readonly toggle?: boolean
  /** Opens its own picker instead of the panel's value list. */
  readonly open?: (signal: AbortSignal) => Promise<CommandResult>
  readonly set: (value: string) => Promise<void>
}

/** The pinned row that clears the user's `tui` section. Not a settings key. */
const RESET = '\u0000reset'

/** Hooks the panel borrows from the session it opens in. */
export interface PanelSession {
  /**
   * Choose the session's model and effort, which also becomes the default
   * for new sessions. Absent, the default model row is not offered.
   */
  readonly chooseModel?: (signal: AbortSignal) => Promise<CommandResult>
}

/** Fixed steps offered for a number, with the current value kept among them. */
function steps(list: readonly number[], current: number, show: (value: number) => string = String): readonly Option[] {
  return [...new Set([...list, current])].sort((left, right) => left - right).map(value => ({ value: String(value), label: show(value) }))
}

/**
 * The `const` choices of one field in a serialized schemastery object schema.
 * @param schema - a descriptor's `schema`, as `toJSON()` wrote it.
 * @param field - the field to read.
 * @returns the choices, with their descriptions; empty when the field is not a union of constants.
 */
export function constChoices(schema: unknown, field: string): readonly { readonly value: string, readonly label: string }[] {
  type Node = { type?: string, value?: unknown, list?: unknown[], dict?: Record<string, unknown>, meta?: { description?: unknown } }
  const refs = (schema as { refs?: Record<string, Node>, uid?: number } | undefined)?.refs
  const root = refs?.[String((schema as { uid?: number }).uid)]
  const node = (ref: unknown): Node | undefined => typeof ref === 'number' ? refs?.[String(ref)] : ref as Node | undefined
  const union = node(root?.dict?.[field])
  if (refs === undefined || union?.type !== 'union') return []
  return (union.list ?? []).flatMap(item => {
    const choice = node(item)
    if (choice?.type !== 'const' || typeof choice.value !== 'string') return []
    const description = choice.meta?.description
    return [{ value: choice.value, label: typeof description === 'string' && description !== '' ? description : choice.value }]
  })
}

/** The `tui` namespace, the values this process launched with, and the `/settings` panel. */
export class Preferences {
  private scope: SettingsScope<TuiSettings> | undefined
  private service: SettingsProvider | undefined
  private local: TuiSettings
  private started: TuiSettings | undefined

  /**
   * @param ctx - the runner's plugin context; the namespace lives as long as it does.
   * @param base - the profile's values, below the user's.
   * @param screenFlag - `--screen` or the profile's explicit `screen`, which the panel cannot override.
   * @param changed - renderer notification, after any committed change, including an edit to the document.
   */
  constructor(private readonly ctx: Context, private readonly base: TuiSettings,
    private readonly screenFlag: TuiSettings['screen'] | undefined, private readonly changed: () => void) {
    this.local = base
    ctx.inject(['settings'], settingsCtx => {
      let scope: SettingsScope<TuiSettings>
      try {
        scope = settingsCtx.settings.register(SETTINGS_NAMESPACE, TuiSettings, { base })
      } catch (error) {
        // A hand-edited section the schema rejects. The profile's values stand.
        ctx.logger.warn('tui: ignoring the settings document\'s "%s" section: %s', SETTINGS_NAMESPACE,
          error instanceof Error ? error.message : String(error))
        return
      }
      this.scope = scope
      this.service = settingsCtx.settings
      const off = scope.watch(() => { this.changed() })
      settingsCtx.effect(() => () => {
        off()
        if (this.scope === scope) this.local = scope.get()
        this.scope = undefined
        this.service = undefined
      }, 'tui settings')
    })
  }

  /** Current values: the settings document's, or this process's own without one. */
  get value(): TuiSettings { return this.scope?.get() ?? this.local }

  /**
   * Values read once at launch, fixed by the first read. Call after the
   * loader settles, so the settings document is read.
   */
  get launch(): TuiSettings { return this.started ??= this.value }

  /** The renderer this process runs, which a flag or the profile may fix. */
  get screen(): TuiSettings['screen'] { return this.screenFlag ?? this.launch.screen }

  /**
   * Change the surface's settings and persist them when a document exists.
   * @param patch - fields to change.
   */
  async update(patch: Partial<TuiSettings>): Promise<void> {
    if (this.scope !== undefined) { await this.scope.update(patch); return }
    this.local = { ...this.local, ...patch }
    this.changed()
  }

  /**
   * Run the `/settings` panel: pick a row, then its value, until Escape.
   * Each pick is saved before the list returns with the pointer where it was;
   * a failed save stays in the panel as its warning instead of closing it.
   * @param copy - locale-owned labels.
   * @param interactions - the session's picker queue.
   * @param signal - the command's lifetime.
   * @param session - hooks into the session the panel opened in.
   * @returns the command's outcome: where the changes went, when there were any.
   */
  async panel(copy: TuiCopy, interactions: Interactions, signal: AbortSignal, session: PanelSession = {}): Promise<CommandResult> {
    let initial = 'screen'
    let problem: string | undefined
    let changed = false
    for (;;) {
      const rows = this.rows(copy, session)
      const path = this.service?.documentPath
      const warnings = [this.scope === undefined ? copy.settingsUnsaved : undefined, problem].filter(text => text !== undefined)
      const picked = await interactions.choose({
        title: path === undefined ? copy.settingsTitle : `${copy.settingsTitle} · ${compactPath(path, process.env['HOME'])}`,
        initial,
        choices: [
          ...rows.map(row => ({
            value: row.key, label: row.label,
            description: row.shown ?? row.options.find(option => option.value === row.current)?.label ?? row.current,
            ...row.status === undefined ? {} : { status: row.status === copy.settingsNextLaunch
              ? { text: row.status, tone: 'waiting' as const } : { text: row.status } },
          })),
          ...this.customized ? [{ value: RESET, label: copy.settingsReset, pinned: true }] : [],
        ],
        ...warnings.length === 0 ? {} : { warning: warnings.join(' · ') },
      }, signal)
      signal.throwIfAborted()
      if (picked === undefined) return this.closing(copy, changed)
      initial = picked
      problem = undefined
      if (picked === RESET) {
        const confirmed = await interactions.choose({
          title: copy.settingsResetConfirm, initial: 'keep',
          choices: [{ value: 'keep', label: copy.settingsResetKeep }, { value: 'reset', label: copy.settingsResetDo }],
        }, signal)
        signal.throwIfAborted()
        if (confirmed !== 'reset') continue
        try {
          await this.reset()
          changed = true
          initial = 'screen'
        } catch (error) { problem = failure(copy, error) }
        continue
      }
      const row = rows.find(entry => entry.key === picked)!
      if (row.open !== undefined) {
        const result = await row.open(signal)
        signal.throwIfAborted()
        if (result.kind === 'error') problem = result.text
        else if (this.rows(copy, session).find(entry => entry.key === row.key)?.shown !== row.shown) changed = true
        continue
      }
      const value = row.toggle === true ? row.options.find(option => option.value !== row.current)?.value
        : await interactions.choose({
          title: row.label, initial: row.current,
          choices: row.options.map(option => ({ value: option.value, label: option.label, current: option.value === row.current,
            ...option.description === undefined ? {} : { description: option.description } })),
        }, signal)
      signal.throwIfAborted()
      if (value === undefined || value === row.current) continue
      try {
        await row.set(value)
        changed = true
      } catch (error) { problem = failure(copy, error) }
    }
  }

  /** Whether the user's `tui` section sets anything the profile does not. */
  private get customized(): boolean {
    if (this.scope === undefined) return (Object.keys(this.base) as (keyof TuiSettings)[]).some(key => this.local[key] !== this.base[key])
    const user = this.service?.describe().find(entry => entry.ns === SETTINGS_NAMESPACE)?.user
    return typeof user === 'object' && user !== null && Object.keys(user).length > 0
  }

  /** Drop the user's `tui` section, so every field re-inherits the profile's. */
  private async reset(): Promise<void> {
    if (this.scope !== undefined) { await this.scope.replace({}); return }
    this.local = this.base
    this.changed()
  }

  /** The line the panel leaves in the transcript as it closes. */
  private closing(copy: TuiCopy, changed: boolean): CommandResult {
    if (!changed) return { kind: 'success' }
    if (this.scope === undefined) return { kind: 'success', text: copy.settingsUnsaved }
    const path = this.service?.documentPath
    return { kind: 'success', text: path === undefined ? copy.settingsStored : `${copy.settingsSaved} ${compactPath(path, process.env['HOME'])}` }
  }

  private rows(copy: TuiCopy, session: PanelSession): readonly Row[] {
    const value = this.value
    const launch = this.launch
    const base = this.base
    const later = (changed: boolean): { status?: string } => changed ? { status: copy.settingsNextLaunch } : {}
    // The profile's value, marked in the value picker so a user can find the way back.
    const marked = (options: readonly Option[], fallback: string): readonly Option[] =>
      options.map(option => option.value === fallback ? { ...option, description: copy.settingsDefault } : option)
    return [
      ...this.modelRow(copy, session),
      ...this.permissionRow(copy),
      {
        key: 'screen', label: copy.settingsScreen, current: value.screen,
        options: marked([{ value: 'inline', label: copy.settingsScreenInline }, { value: 'fullscreen', label: copy.settingsScreenFullscreen }], base.screen),
        ...this.screenFlag !== undefined ? { status: copy.settingsByFlag } : later(value.screen !== launch.screen),
        set: next => this.update({ screen: next as TuiSettings['screen'] }),
      },
      {
        key: 'locale', label: copy.settingsLocale, current: value.locale,
        options: marked([{ value: 'en', label: 'English' }, { value: 'zh', label: '中文' }], base.locale),
        ...later(value.locale !== launch.locale),
        set: next => this.update({ locale: next as TuiSettings['locale'] }),
      },
      {
        key: 'composerFrame', label: copy.settingsFrame, current: value.composerFrame,
        options: marked([{ value: 'auto', label: copy.settingsFrameAuto }, { value: 'round', label: copy.settingsFrameRound },
          { value: 'classic', label: copy.settingsFrameClassic }], base.composerFrame),
        set: next => this.update({ composerFrame: next as TuiSettings['composerFrame'] }),
      },
      {
        key: 'goalObjective', label: copy.settingsGoalObjective, current: String(value.goalObjective), toggle: true,
        options: [{ value: 'false', label: copy.settingsOff }, { value: 'true', label: copy.settingsOn }],
        set: next => this.update({ goalObjective: next === 'true' }),
      },
      {
        key: 'resultLines', label: copy.settingsResultLines, current: String(value.resultLines),
        options: marked(steps([0, 2, 4, 8, 16, 32], value.resultLines, lines => lines === 0 ? copy.settingsResultLinesNone : String(lines)),
          String(base.resultLines)),
        set: next => this.update({ resultLines: Number(next) }),
      },
      {
        key: 'completionLimit', label: copy.settingsCompletionLimit, current: String(value.completionLimit),
        options: marked(steps([4, 6, 8, 12, 16], value.completionLimit), String(base.completionLimit)),
        set: next => this.update({ completionLimit: Number(next) }),
      },
      {
        key: 'doubleInterruptMs', label: copy.settingsDoubleInterrupt, current: String(value.doubleInterruptMs),
        options: marked(steps([1000, 2000, 3000, 5000], value.doubleInterruptMs, ms => `${ms / 1000}s`), String(base.doubleInterruptMs)),
        set: next => this.update({ doubleInterruptMs: Number(next) }),
      },
    ]
  }

  /**
   * The model new sessions start on. Choosing it runs the session's own
   * model picker, which switches this session too and saves the choice.
   */
  private modelRow(copy: TuiCopy, session: PanelSession): readonly Row[] {
    const defaults = this.ctx.get('agentDefaultModel')
    const open = session.chooseModel
    if (defaults === undefined || open === undefined) return []
    const selected = defaults.currentSelection()
    const route = `${selected.provider}/${selected.model}`
    return [{
      key: 'defaultModel', label: copy.settingsModel, current: route, options: [],
      shown: selected.reasoningEffort === undefined ? route : `${route} (${String(selected.reasoningEffort)})`,
      status: copy.settingsNewSessions, open,
      set: () => Promise.resolve(),
    }]
  }

  /** The default access preset, when the permission plugin registered its namespace. */
  private permissionRow(copy: TuiCopy): readonly Row[] {
    const service = this.service
    const descriptor = service?.describe().find(entry => entry.ns === PERMISSION_SETTINGS_NAMESPACE)
    const current = (descriptor?.value as { defaultPreset?: unknown } | undefined)?.defaultPreset
    const options = descriptor === undefined ? [] : constChoices(descriptor.schema, 'defaultPreset')
    if (service === undefined || typeof current !== 'string' || options.length === 0) return []
    return [{
      key: 'defaultPreset', label: copy.settingsPermission, current,
      options: options.map(option => ({ value: option.value, label: option.value,
        ...option.label === option.value ? {} : { description: option.label } })),
      status: copy.settingsNewSessions,
      set: next => service.update(PERMISSION_SETTINGS_NAMESPACE, { defaultPreset: next }),
    }]
  }
}

/** A failed save, as the panel's warning names it. */
function failure(copy: TuiCopy, error: unknown): string {
  return `${copy.settingsFailed}: ${error instanceof Error ? error.message : String(error)}`
}
