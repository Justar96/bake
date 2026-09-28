/**
 * The terminal's user settings: the `tui` namespace of the settings document,
 * and the `/settings` panel that edits it beside what other plugins register
 * there.
 *
 * The panel lists sections. Session holds the default model (the session's
 * `/model` picker, which writes `agent-default-model`) and the default access
 * preset. Terminal holds this namespace. Agent, Shell, and Web search name the
 * plugin settings a user reaches for most, with labels and steps of their
 * own. Advanced lists every registered namespace and edits its fields from
 * the schema its owner registered, so a plugin's settings are reachable
 * without the panel knowing the plugin. Typing at the top searches every
 * setting in every section.
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
import type { SettingsDescriptor, SettingsProvider, SettingsScope } from '@deepseek-ai/dsh-settings'
import type { CommandResult } from '@deepseek-ai/dsh-commands'
import type {} from '@deepseek-ai/dsh-agent-default-model'
import { PERMISSION_SETTINGS_NAMESPACE } from '@deepseek-ai/dsh-permission-presets'
import type { TuiCopy } from '@dsh-tui/ui/copy.ts'
import type { Choice } from '@dsh-tui/ui/picker.tsx'
import { compactPath } from '@dsh-tui/ui/present.ts'
import type { Interactions } from './interactions.ts'
import { formatBytes, formatDuration, parseNumber, schemaFields, type SchemaField } from './schema-fields.ts'

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

/** One value a setting offers. */
interface Option {
  readonly value: string
  readonly label: string
  /** Secondary text in the value picker. */
  readonly description?: string
}

/** How a typed value is read, for a setting that takes one. */
interface Typed {
  readonly kind: 'number' | 'string' | 'secret'
  readonly min?: number
  readonly max?: number
  readonly step?: number
  /** A number's unit, which also decides the suffixes it accepts. */
  readonly unit?: 'ms' | 'bytes'
}

/** One setting of a section, with the values it offers. */
interface Setting {
  readonly key: string
  readonly label: string
  /** The current value, as an option's `value`. */
  readonly current: string
  /** What the row shows for its value. */
  readonly shown: string
  readonly options: readonly Option[]
  /** When the change takes effect, when that is not now. */
  readonly status?: string
  /** Two options, so Enter flips it instead of opening a list. */
  readonly toggle?: boolean
  /** Also accepts a typed value. */
  readonly typed?: Typed
  /** The owner's description, beside the label in the value picker's title. */
  readonly about?: string
  /** Opens its own picker instead of the panel's value list. */
  readonly open?: (signal: AbortSignal) => Promise<CommandResult>
  /** A list or a map: the settings file edits it, this panel does not. */
  readonly inFile?: boolean
  readonly set: (value: string) => Promise<void>
  /** Remove the user's value, so the default returns; absent while there is none. */
  readonly reset?: () => Promise<void>
}

/** One page of the panel: settings, sections under it, or both. */
interface Section {
  readonly key: string
  readonly label: string
  /** The row's text on the page above; absent, the first values. */
  readonly summary?: string
  readonly status?: string
  readonly settings: readonly Setting[]
  readonly sections?: readonly Section[]
}

/** Values the panel's pickers return that are not a setting or a section. */
const RESET = '\u0000reset'
const CUSTOM = '\u0000custom'
const SECTION = 'section:'
const SETTING = 'setting:'

/** What one run of the panel has done so far. */
interface Progress {
  changed: boolean
  problem: string | undefined
}

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

/** A plugin setting the panel names itself, read from its owner's schema. */
interface Curated {
  readonly ns: string
  readonly path: readonly string[]
  readonly label: string
  /** Values offered before the typed one. */
  readonly steps?: readonly number[]
  readonly unit?: 'ms' | 'bytes'
  /** How a number reads; absent, by its unit. */
  readonly format?: (value: number) => string
  readonly status?: string
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
   * Run the `/settings` panel: pick a section, then a setting, then its value,
   * until Escape leaves the top. Each pick is saved before the list returns
   * with the pointer where it was; a failed save stays in the panel as its
   * warning instead of closing it.
   * @param copy - locale-owned labels.
   * @param interactions - the session's picker queue.
   * @param signal - the command's lifetime.
   * @param session - hooks into the session the panel opened in.
   * @returns the command's outcome: where the changes went, when there were any.
   */
  async panel(copy: TuiCopy, interactions: Interactions, signal: AbortSignal, session: PanelSession = {}): Promise<CommandResult> {
    const progress: Progress = { changed: false, problem: undefined }
    await this.page([], copy, interactions, signal, session, progress)
    return this.closing(copy, progress.changed)
  }

  /**
   * One page, until Escape returns to the page above. The top page also
   * offers every setting below it to a search.
   * @param keys - the sections leading to the page; empty for the top.
   */
  private async page(keys: readonly string[], copy: TuiCopy, interactions: Interactions, signal: AbortSignal,
    session: PanelSession, progress: Progress): Promise<void> {
    let initial: string | undefined
    for (;;) {
      // Read again each time, so a row shows what the last pick saved.
      const top = this.sections(copy, session)
      const trail = locate(top, keys)
      if (trail === undefined) return
      const page = trail.at(-1)
      const sections = page === undefined ? top : page.sections ?? []
      const settings = page?.settings ?? []
      const resettable = settings.filter(setting => setting.reset !== undefined)
      const path = this.service?.documentPath
      const title = page === undefined
        ? path === undefined ? copy.settingsTitle : `${copy.settingsTitle} · ${compactPath(path, process.env['HOME'])}`
        : [copy.settingsTitle, ...trail.map(section => section.label)].join(' › ')
      const warnings = [this.scope === undefined ? copy.settingsUnsaved : undefined, progress.problem].filter(text => text !== undefined)
      const choices: Choice[] = [
        ...settings.map(setting => settingChoice(setting, copy, `${SETTING}${setting.key}`)),
        // Before the sections, so a search lists the settings it found ahead
        // of a section whose summary merely names them.
        ...page === undefined ? searchable(top, copy) : [],
        ...sections.map(section => ({
          value: `${SECTION}${section.key}`, label: section.label, description: summaryOf(section),
          ...section.status === undefined ? {} : { status: statusOf(section.status, copy) },
        })),
        ...resettable.length === 0 ? [] : [{ value: RESET, label: copy.settingsResetSection, pinned: true }],
      ]
      const listed = choices.filter(choice => choice.searchOnly !== true)
      if (listed.length === 0) return
      const picked = await interactions.choose({
        title, choices, initial: initial !== undefined && listed.some(choice => choice.value === initial) ? initial : listed[0]!.value,
        ...warnings.length === 0 ? {} : { warning: warnings.join(' · ') },
      }, signal)
      signal.throwIfAborted()
      if (picked === undefined) return
      progress.problem = undefined
      if (picked === RESET) {
        initial = settings[0] === undefined ? undefined : `${SETTING}${settings[0].key}`
        const confirmed = await interactions.choose({
          title: copy.settingsResetConfirm, initial: 'keep',
          choices: [{ value: 'keep', label: copy.settingsResetKeep }, { value: 'reset', label: copy.settingsResetDo }],
        }, signal)
        signal.throwIfAborted()
        if (confirmed !== 'reset') continue
        await save(copy, progress, async () => { for (const setting of resettable) await setting.reset!() })
        continue
      }
      if (picked.startsWith(SECTION)) {
        initial = picked
        await this.page([...keys, picked.slice(SECTION.length)], copy, interactions, signal, session, progress)
        continue
      }
      // A search result names its sections before the setting.
      const found = picked.slice(SETTING.length).split('/')
      const at = found.length > 1 ? locate(top, found.slice(0, -1))?.at(-1) : page
      const setting = at?.settings.find(entry => entry.key === found.at(-1))
      initial = found.length > 1 ? `${SECTION}${found[0]}` : picked
      if (setting === undefined) continue
      await this.edit(setting, copy, interactions, signal, progress)
      // A setting with its own picker saves through it; the row says whether it did.
      if (setting.open !== undefined && progress.problem === undefined) {
        const again = locate(this.sections(copy, session), found.length > 1 ? found.slice(0, -1) : keys)
        const now = (found.length > 1 || keys.length > 0 ? again?.at(-1)?.settings : [])?.find(entry => entry.key === setting.key)
        if (now !== undefined && now.shown !== setting.shown) progress.changed = true
      }
    }
  }

  /** Change one setting: flip it, open its picker, or offer its values. */
  private async edit(setting: Setting, copy: TuiCopy, interactions: Interactions, signal: AbortSignal, progress: Progress): Promise<void> {
    if (setting.inFile) {
      const path = this.service?.documentPath
      progress.problem = path === undefined ? copy.settingsInFile : `${copy.settingsInFile}: ${compactPath(path, process.env['HOME'])}`
      return
    }
    if (setting.open !== undefined) {
      const result = await setting.open(signal)
      signal.throwIfAborted()
      if (result.kind === 'error') progress.problem = result.text
      return
    }
    if (setting.toggle === true) {
      const next = setting.options.find(option => option.value !== setting.current)?.value
      if (next !== undefined) await save(copy, progress, () => setting.set(next))
      return
    }
    const choices: Choice[] = [
      ...setting.options.map(option => ({ value: option.value, label: option.label, current: option.value === setting.current,
        ...option.description === undefined ? {} : { description: option.description } })),
      ...setting.typed === undefined ? [] : [{ value: CUSTOM, label: copy.settingsCustom,
        ...hintOf(setting.typed, copy) === undefined ? {} : { description: hintOf(setting.typed, copy)! } }],
      ...setting.reset === undefined ? [] : [{ value: RESET, label: copy.settingsResetField, pinned: true }],
    ]
    // Nothing to pick from but typing: ask for the text at once.
    const value = choices.length === 1 && choices[0]!.value === CUSTOM ? CUSTOM : await interactions.choose({
      title: setting.about === undefined ? setting.label : `${setting.label} · ${setting.about}`,
      initial: choices.some(choice => choice.value === setting.current) ? setting.current : choices[0]!.value,
      choices,
    }, signal)
    signal.throwIfAborted()
    if (value === undefined) return
    if (value === RESET) { await save(copy, progress, setting.reset!); return }
    if (value !== CUSTOM) {
      if (value !== setting.current) await save(copy, progress, () => setting.set(value))
      return
    }
    const typed = setting.typed!
    let text: string
    try {
      text = await interactions.prompt({
        kind: typed.kind === 'secret' ? 'secret' : 'text',
        message: [setting.label, hintOf(typed, copy)].filter(part => part !== undefined).join(' · '),
      }, signal)
    } catch (error) {
      // Escape leaves the value as it was.
      if (signal.aborted) throw error
      return
    }
    if (typed.kind !== 'number') { await save(copy, progress, () => setting.set(text)); return }
    const read = parseNumber(text, typed, typed.unit)
    if ('problem' in read) { progress.problem = `${setting.label}: ${problemText(read.problem, typed, copy)}`; return }
    await save(copy, progress, () => setting.set(String(read.value)))
  }

  /** The line the panel leaves in the transcript as it closes. */
  private closing(copy: TuiCopy, changed: boolean): CommandResult {
    if (!changed) return { kind: 'success' }
    if (this.scope === undefined) return { kind: 'success', text: copy.settingsUnsaved }
    const path = this.service?.documentPath
    return { kind: 'success', text: path === undefined ? copy.settingsStored : `${copy.settingsSaved} ${compactPath(path, process.env['HOME'])}` }
  }

  /** Every section the running composition can fill, in the panel's order. */
  private sections(copy: TuiCopy, session: PanelSession): readonly Section[] {
    const descriptors = this.service?.describe() ?? []
    const curated = (list: readonly Curated[]): readonly Setting[] => list.flatMap(entry => this.curated(entry, descriptors, copy))
    const section = (key: string, label: string, settings: readonly Setting[], named = false): readonly Section[] =>
      settings.length === 0 ? [] : [{ key, label, settings,
        // A bare number says nothing on the page above, so these sections name what they hold.
        ...named ? { summary: settings.map(setting => setting.label).join(' · ') } : {} }]
    return [
      ...section('session', copy.settingsSession, [...this.modelRow(copy, session), ...this.permissionRow(copy, descriptors)]),
      { key: 'terminal', label: copy.settingsTerminal, settings: this.terminalRows(copy) },
      ...section('agent', copy.settingsAgent, curated([
        { ns: 'agent-loop', path: ['maxParallelToolCalls'], label: copy.settingsParallelTools, steps: [1, 2, 4, 8, 16] },
        { ns: 'subagent', path: ['maxActiveSubagents'], label: copy.settingsSubagentsActive, steps: [1, 2, 4, 8, 16] },
        { ns: 'subagent', path: ['maxDepth'], label: copy.settingsSubagentDepth, steps: [0, 1, 2, 3],
          format: depth => depth === 0 ? copy.settingsSubagentDepthOff : String(depth) },
        { ns: 'subagent-model-selection', path: ['enabled'], label: copy.settingsSubagentModels, status: copy.settingsNewSessions },
      ]), true),
      ...section('shell', copy.settingsShell, curated([
        { ns: 'shell', path: ['timeoutMs'], label: copy.settingsShellTimeout, unit: 'ms', steps: [30_000, 60_000, 120_000, 300_000, 600_000] },
        { ns: 'shell', path: ['maxTimeoutMs'], label: copy.settingsShellMaxTimeout, unit: 'ms', steps: [300_000, 600_000, 1_800_000, 3_600_000] },
        { ns: 'shell', path: ['maxOutputBytes'], label: copy.settingsShellOutput, unit: 'bytes', steps: [16_000, 64_000, 256_000, 1_000_000] },
      ]), true),
      ...section('web', copy.settingsWeb, curated([
        { ns: 'web-search-deepseek', path: ['maxUses'], label: copy.settingsWebUses, steps: [1, 3, 5, 10] },
        { ns: 'web-search-deepseek', path: ['model'], label: copy.settingsWebModel },
        { ns: 'web-search-deepseek', path: ['maxTokens'], label: copy.settingsWebTokens, steps: [1024, 2048, 4096, 8192] },
      ]), true),
      ...this.advanced(copy, descriptors),
    ]
  }

  /** This namespace's rows. Each carries a reset once the user's section sets it. */
  private terminalRows(copy: TuiCopy): readonly Setting[] {
    const value = this.value
    const launch = this.launch
    const base = this.base
    const later = (changed: boolean): { status?: string } => changed ? { status: copy.settingsNextLaunch } : {}
    // The profile's value, marked in the value picker so a user can find the way back.
    const marked = (options: readonly Option[], fallback: string): readonly Option[] =>
      options.map(option => option.value === fallback ? { ...option, description: copy.settingsDefault } : option)
    const row = <K extends keyof TuiSettings>(key: K, fields: Omit<Setting, 'key' | 'shown' | 'set' | 'reset'> & { shown?: string },
      read: (next: string) => TuiSettings[K]): Setting => ({
      ...fields, key,
      shown: fields.shown ?? fields.options.find(option => option.value === fields.current)?.label ?? fields.current,
      set: next => this.update({ [key]: read(next) }),
      ...this.overridden(key) ? { reset: () => this.unset(key) } : {},
    })
    return [
      row('screen', {
        label: copy.settingsScreen, current: value.screen,
        options: marked([{ value: 'inline', label: copy.settingsScreenInline }, { value: 'fullscreen', label: copy.settingsScreenFullscreen }], base.screen),
        ...this.screenFlag !== undefined ? { status: copy.settingsByFlag } : later(value.screen !== launch.screen),
      }, next => next as TuiSettings['screen']),
      row('locale', {
        label: copy.settingsLocale, current: value.locale,
        options: marked([{ value: 'en', label: 'English' }, { value: 'zh', label: '中文' }], base.locale),
        ...later(value.locale !== launch.locale),
      }, next => next as TuiSettings['locale']),
      row('composerFrame', {
        label: copy.settingsFrame, current: value.composerFrame,
        options: marked([{ value: 'auto', label: copy.settingsFrameAuto }, { value: 'round', label: copy.settingsFrameRound },
          { value: 'classic', label: copy.settingsFrameClassic }], base.composerFrame),
      }, next => next as TuiSettings['composerFrame']),
      row('goalObjective', {
        label: copy.settingsGoalObjective, current: String(value.goalObjective), toggle: true,
        options: [{ value: 'false', label: copy.settingsOff }, { value: 'true', label: copy.settingsOn }],
      }, next => next === 'true'),
      row('resultLines', {
        label: copy.settingsResultLines, current: String(value.resultLines), typed: { kind: 'number', min: 0, step: 1 },
        options: marked(steps([0, 2, 4, 8, 16, 32], value.resultLines, lines => lines === 0 ? copy.settingsResultLinesNone : String(lines)),
          String(base.resultLines)),
      }, Number),
      row('completionLimit', {
        label: copy.settingsCompletionLimit, current: String(value.completionLimit), typed: { kind: 'number', min: 1, step: 1 },
        options: marked(steps([4, 6, 8, 12, 16], value.completionLimit), String(base.completionLimit)),
      }, Number),
      row('doubleInterruptMs', {
        label: copy.settingsDoubleInterrupt, current: String(value.doubleInterruptMs), typed: { kind: 'number', min: 1, unit: 'ms' },
        options: marked(steps([1000, 2000, 3000, 5000], value.doubleInterruptMs, formatDuration), String(base.doubleInterruptMs)),
      }, Number),
    ]
  }

  /** Whether the user's `tui` section sets `key`, or, without a document, this process moved it off the profile's. */
  private overridden(key: keyof TuiSettings): boolean {
    if (this.scope === undefined) return this.local[key] !== this.base[key]
    const user = this.service?.describe().find(entry => entry.ns === SETTINGS_NAMESPACE)?.user
    return typeof user === 'object' && user !== null && Object.hasOwn(user, key)
  }

  /** Drop one field of the user's `tui` section, so it re-inherits the profile's. */
  private async unset(key: keyof TuiSettings): Promise<void> {
    if (this.scope !== undefined && this.service !== undefined) {
      await this.service.mutate(SETTINGS_NAMESPACE, [{ op: 'unset', path: [key] }])
      return
    }
    this.local = { ...this.local, [key]: this.base[key] }
    this.changed()
  }

  /**
   * The model new sessions start on. Choosing it runs the session's own
   * model picker, which switches this session too and saves the choice.
   */
  private modelRow(copy: TuiCopy, session: PanelSession): readonly Setting[] {
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
  private permissionRow(copy: TuiCopy, descriptors: readonly SettingsDescriptor[]): readonly Setting[] {
    const descriptor = descriptors.find(entry => entry.ns === PERMISSION_SETTINGS_NAMESPACE)
    const field = descriptor === undefined ? undefined
      : schemaFields(descriptor.schema, descriptor.value, descriptor.user).find(entry => entry.path.join('.') === 'defaultPreset')
    if (field?.kind !== 'choice' || typeof field.value !== 'string') return []
    return [{
      ...this.fieldSetting(PERMISSION_SETTINGS_NAMESPACE, field, copy), key: 'defaultPreset', label: copy.settingsPermission,
      options: field.choices!.map(option => ({ value: option.value, label: option.value,
        ...option.label === option.value ? {} : { description: option.label } })),
      shown: field.value, status: copy.settingsNewSessions,
    }]
  }

  /** A curated plugin setting, when its namespace is registered and its field is one the panel edits. */
  private curated(entry: Curated, descriptors: readonly SettingsDescriptor[], copy: TuiCopy): readonly Setting[] {
    const descriptor = descriptors.find(candidate => candidate.ns === entry.ns)
    const field = descriptor === undefined ? undefined
      : schemaFields(descriptor.schema, descriptor.value, descriptor.user).find(candidate => candidate.path.join('.') === entry.path.join('.'))
    if (descriptor === undefined || field === undefined || field.kind === 'other') return []
    const setting = this.fieldSetting(entry.ns, field, copy, descriptor.applies === 'restart')
    const status = entry.status ?? setting.status
    if (field.kind !== 'number' || typeof field.value !== 'number') {
      return [{ ...setting, key: `${entry.ns}.${entry.path.join('.')}`, label: entry.label, ...status === undefined ? {} : { status } }]
    }
    const show = entry.format ?? (entry.unit === 'ms' ? formatDuration : entry.unit === 'bytes' ? formatBytes : String)
    const fallback = typeof field.default === 'number' ? String(field.default) : undefined
    return [{
      ...setting, key: `${entry.ns}.${entry.path.join('.')}`, label: entry.label, shown: show(field.value),
      options: steps((entry.steps ?? []).filter(value => (field.min === undefined || value >= field.min) && (field.max === undefined || value <= field.max)),
        field.value, show).map(option => option.value === fallback ? { ...option, description: copy.settingsDefault } : option),
      typed: { kind: 'number', ...bounds(field), ...entry.unit === undefined ? {} : { unit: entry.unit } },
      ...status === undefined ? {} : { status },
    }]
  }

  /** Advanced: every registered namespace but this one, each field as its schema describes it. */
  private advanced(copy: TuiCopy, descriptors: readonly SettingsDescriptor[]): readonly Section[] {
    const sections = descriptors
      .filter(descriptor => descriptor.ns !== SETTINGS_NAMESPACE)
      .map(descriptor => {
        const restart = descriptor.applies === 'restart'
        const settings = schemaFields(descriptor.schema, descriptor.value, descriptor.user)
          .map(field => this.fieldSetting(descriptor.ns, field, copy, restart))
        return {
          key: descriptor.ns, label: descriptor.ns, settings,
          summary: `${settings.length} ${settings.length === 1 ? copy.settingsField : copy.settingsFields}`,
          ...restart ? { status: copy.settingsNextLaunch } : {},
        }
      })
      .filter(section => section.settings.length > 0)
      .sort((left, right) => left.key.localeCompare(right.key))
    return sections.length === 0 ? [] : [{ key: 'advanced', label: copy.settingsAdvanced, summary: copy.settingsAdvancedAbout, settings: [], sections }]
  }

  /**
   * One schema field as a setting, labelled by its path. Every write names
   * the field alone, so a secret the panel never read is never lost.
   */
  private fieldSetting(ns: string, field: SchemaField, copy: TuiCopy, restart = false): Setting {
    const service = this.service!
    const key = field.path.join('.')
    const write = (value: unknown): Promise<void> => service.mutate(ns, [{ op: 'set', path: field.path, value }])
    const common = {
      key, label: key, ...field.description === undefined ? {} : { about: field.description },
      ...restart ? { status: copy.settingsNextLaunch } : {},
      ...field.overridden ? { reset: () => service.mutate(ns, [{ op: 'unset', path: field.path }]) } : {},
    }
    const fallback = (options: readonly Option[], value: unknown): readonly Option[] => options
      .map(option => option.value === String(value) ? { ...option, description: copy.settingsDefault } : option)
    switch (field.kind) {
      case 'boolean': return {
        ...common, current: String(field.value === true), toggle: true, shown: field.value === true ? copy.settingsOn : copy.settingsOff,
        options: [{ value: 'false', label: copy.settingsOff }, { value: 'true', label: copy.settingsOn }],
        set: next => write(next === 'true'),
      }
      case 'choice': return {
        ...common, current: String(field.value), shown: field.choices!.find(option => option.value === field.value)?.label ?? String(field.value),
        options: fallback(field.choices!, field.default), set: next => write(next),
      }
      case 'number': {
        const known = [field.value, field.default].filter((value): value is number => typeof value === 'number')
        return {
          ...common, current: String(field.value), shown: field.value === undefined ? copy.settingsEmpty : String(field.value),
          options: fallback(known.length === 0 ? [] : steps(known.slice(1), known[0]!), field.default),
          typed: { kind: 'number', ...bounds(field) }, set: next => write(Number(next)),
        }
      }
      case 'string': {
        const shown = typeof field.value === 'string' && field.value !== '' ? field.value : copy.settingsEmpty
        const options = [...new Set([field.value, field.default].filter((value): value is string => typeof value === 'string' && value !== ''))]
          .map(value => ({ value, label: value }))
        return { ...common, current: String(field.value ?? ''), shown, options: fallback(options, field.default), typed: { kind: 'string' }, set: next => write(next) }
      }
      case 'secret': return {
        ...common, current: '', options: [], typed: { kind: 'secret' },
        shown: typeof field.value === 'string' && field.value !== '' ? copy.settingsSecretSet : copy.settingsSecretUnset,
        set: next => write(next),
      }
      case 'other': return {
        ...common, current: '', options: [], inFile: true, shown: `${preview(field.value)} · ${copy.settingsInFile}`,
        set: () => Promise.resolve(),
      }
    }
  }
}

/**
 * The sections a list of keys leads through.
 * @returns each section on the way, or undefined when one is no longer there.
 */
function locate(top: readonly Section[], keys: readonly string[]): readonly Section[] | undefined {
  const trail: Section[] = []
  let level = top
  for (const key of keys) {
    const next = level.find(section => section.key === key)
    if (next === undefined) return undefined
    trail.push(next)
    level = next.sections ?? []
  }
  return trail
}

/** Every setting below the top, labelled by where it lives, for the top page's search. */
function searchable(top: readonly Section[], copy: TuiCopy): readonly Choice[] {
  const walk = (sections: readonly Section[], keys: readonly string[], labels: readonly string[]): Choice[] => sections.flatMap(section => [
    ...section.settings.map(setting => ({
      ...settingChoice(setting, copy, `${SETTING}${[...keys, section.key, setting.key].join('/')}`),
      label: [...labels, section.label, setting.label].join(' › '), searchOnly: true,
    })),
    ...walk(section.sections ?? [], [...keys, section.key], [...labels, section.label]),
  ])
  return walk(top, [], [])
}

/** A setting's row. */
function settingChoice(setting: Setting, copy: TuiCopy, value: string): Choice {
  return {
    value, label: setting.label, description: setting.shown,
    ...setting.status === undefined ? {} : { status: statusOf(setting.status, copy) },
  }
}

/** A row's status. The wait for a restart is the one a user acts on, so it is coloured. */
function statusOf(status: string, copy: TuiCopy): { readonly text: string, readonly tone?: 'waiting' } {
  return status === copy.settingsNextLaunch ? { text: status, tone: 'waiting' } : { text: status }
}

/** A section's row text: its own summary, or its first values. */
function summaryOf(section: Section): string {
  return section.summary ?? section.settings.slice(0, 3).map(setting => setting.shown).join(' · ')
}

/** What the typed-value row says it accepts. */
function hintOf(typed: Typed, copy: TuiCopy): string | undefined {
  if (typed.unit === 'ms') return copy.settingsDurationHint
  if (typed.unit === 'bytes') return copy.settingsBytesHint
  if (typed.kind !== 'number') return undefined
  const parts = [
    typed.min === undefined ? undefined : `${copy.settingsBelowMin} ${typed.min}`,
    typed.max === undefined || typed.max >= Number.MAX_SAFE_INTEGER ? undefined : `${copy.settingsAboveMax} ${typed.max}`,
  ].filter(part => part !== undefined)
  return parts.length === 0 ? undefined : parts.join(', ')
}

/** Why a typed number was refused. */
function problemText(problem: 'number' | 'min' | 'max' | 'step', typed: Typed, copy: TuiCopy): string {
  switch (problem) {
    case 'number': return copy.settingsNotNumber
    case 'min': return `${copy.settingsBelowMin} ${typed.unit === 'ms' ? formatDuration(typed.min!) : typed.unit === 'bytes' ? formatBytes(typed.min!) : typed.min}`
    case 'max': return `${copy.settingsAboveMax} ${typed.unit === 'ms' ? formatDuration(typed.max!) : typed.unit === 'bytes' ? formatBytes(typed.max!) : typed.max}`
    case 'step': return `${copy.settingsNotStep} ${typed.step}`
  }
}

/** A field's bounds, for its typed value. */
function bounds(field: SchemaField): Pick<Typed, 'min' | 'max' | 'step'> {
  return {
    ...field.min === undefined ? {} : { min: field.min },
    ...field.max === undefined ? {} : { max: field.max },
    ...field.step === undefined ? {} : { step: field.step },
  }
}

/** A list or map, short enough for a row. */
function preview(value: unknown): string {
  if (Array.isArray(value)) return `[${value.length}]`
  if (typeof value === 'object' && value !== null) return `{${Object.keys(value).length}}`
  return value === undefined ? '' : String(value)
}

/** Run a save, and keep its failure as the panel's warning. */
async function save(copy: TuiCopy, progress: Progress, write: () => Promise<void>): Promise<void> {
  try {
    await write()
    progress.changed = true
  } catch (error) { progress.problem = failure(copy, error) }
}

/** A failed save, as the panel's warning names it. */
function failure(copy: TuiCopy, error: unknown): string {
  return `${copy.settingsFailed}: ${error instanceof Error ? error.message : String(error)}`
}
