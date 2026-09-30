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
 * setting in every section, and Tab and Shift-Tab move between the sections.
 *
 * A list or a map opens in the user's editor as JSON, when the runner can
 * hand it the terminal; otherwise the panel names the settings file.
 *
 * Agent also chooses the models subagents may pick from the harness's model
 * catalog, the one `/model` lists. The owner refuses the choice switched on
 * with no model allowed, so switching it on from an empty list asks for the
 * models first and saves both together.
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
import type { EditText } from './external-editor.ts'
import type { Interactions } from './interactions.ts'
import type { LoginPrompt } from './login.ts'
import { parseJsonc, valueOf } from './jsonc.ts'
import { namesRoute, withRecent, type ModelCatalog, type RecentModels } from './model.ts'
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
  /** Models chosen on `/model`, most recent first, which its sheet leads with. The panel does not list it. */
  readonly recentModels: string[]
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
  recentModels: z.array(z.string()).default([]),
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
  /**
   * A list or a map: the user's editor edits it as JSON under this file
   * name, or, without an editor, the settings file does. `current` is its
   * JSON and `set` takes the edited text.
   */
  readonly inFile?: { readonly name: string }
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
const TAB = 'tab:'
const DONE = '\u0000done'

/** The subagent plugin's model-selection namespace, which the Agent section edits as a pair. */
const SUBAGENT_MODELS = 'subagent-model-selection'

/** One exact provider/model route, as the subagent plugin stores it. */
interface ModelRoute {
  readonly provider: string
  readonly model: string
}

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
  /**
   * Every model the harness's providers advertise. Absent, the models
   * subagents may use are edited in the settings file.
   */
  readonly listModels?: (signal: AbortSignal) => Promise<ModelCatalog | undefined>
  /**
   * The task router's view of each model new Sessions may use. Absent, the
   * model calibration list is not offered.
   */
  readonly describeRoutes?: (signal: AbortSignal) => Promise<readonly RouteView[]>
  /** Signing in to the task router. Absent, the router's token is set in the environment. */
  readonly routerAccount?: RouterAccountControls
}

/** Whether Bake holds a router token, as the subagent plugin reports it; never the token. */
export interface RouterTokenView {
  readonly tokenEnv: string
  readonly configured: boolean
  readonly source?: string
  /** Whether signing in or out can change it; false while the launch environment supplies it. */
  readonly writable: boolean
}

/** The task router's email sign-in, which stores the token it issues. */
export interface RouterAccountControls {
  readonly status: () => Promise<RouterTokenView>
  /** The signed-in account's address, asked of the router. */
  readonly account: (signal: AbortSignal) => Promise<string | undefined>
  readonly requestCode: (email: string, signal: AbortSignal) => Promise<void>
  /** @returns the signed-in account's address. */
  readonly signIn: (email: string, code: string, signal: AbortSignal) => Promise<string>
  readonly signOut: (signal: AbortSignal) => Promise<void>
}

/** What the task router knows about one allowed model, as the subagent plugin reports it. */
export interface RouteView extends ModelRoute {
  /** The benchmarked model the router matched. */
  readonly profile?: string
  /** Whether the router has quality evidence for it. */
  readonly ranked: boolean
  /** Overall quality from 0 to 1. */
  readonly quality?: number
  /** Where the quality comes from; benchmarks win over a declared tier. */
  readonly qualitySource?: 'benchmarks' | 'hint'
  /** Blended USD per million tokens. */
  readonly price?: number
}

/** The user's declarations about one allowed model, sent to the router with it. */
interface RouteHint extends ModelRoute {
  readonly sameAs?: string
  readonly quality?: string
  readonly cost?: string
}

/** The task router's stored settings, as the panel edits them. */
interface RouterView {
  readonly enabled: boolean
  readonly url: string
  readonly tokenEnv: string
  readonly priority?: string
  readonly hints: readonly RouteHint[]
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
export class Preferences implements RecentModels {
  private scope: SettingsScope<TuiSettings> | undefined
  private service: SettingsProvider | undefined
  private local: TuiSettings
  private started: TuiSettings | undefined
  /** The router token's state for the open panel, and the account it belongs to once asked. */
  private routerToken: { readonly status: RouterTokenView; readonly email?: string } | undefined

  /**
   * @param ctx - the runner's plugin context; the namespace lives as long as it does.
   * @param base - the profile's values, below the user's.
   * @param screenFlag - `--screen` or the profile's explicit `screen`, which the panel cannot override.
   * @param changed - renderer notification, after any committed change, including an edit to the document.
   * @param editText - the user's editor, borrowing the terminal; absent, lists and maps name the settings file.
   */
  constructor(private readonly ctx: Context, private readonly base: TuiSettings,
    private readonly screenFlag: TuiSettings['screen'] | undefined, private readonly changed: () => void,
    private readonly editText?: EditText) {
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

  get recentModels(): readonly string[] { return this.value.recentModels }

  async rememberModel(route: string): Promise<void> {
    const next = withRecent(this.value.recentModels, route)
    if (next.length === this.value.recentModels.length && next.every((entry, at) => entry === this.value.recentModels[at])) return
    await this.update({ recentModels: [...next] })
  }

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
    // Rows are drawn synchronously, so the router account's local state is read once up front.
    // A failed read only leaves the account row saying it is signed out.
    const account = session.routerAccount
    this.routerToken = account === undefined ? undefined
      : await Promise.resolve().then(account.status).then(status => ({ status }), () => undefined)
    await this.page([], copy, interactions, signal, session, progress)
    return this.closing(copy, progress.changed)
  }

  /**
   * One page, until Escape returns to the page above. The top page also
   * offers every setting below it to a search. The top page and each section
   * directly under it carry the sections as tabs.
   * @param keys - the sections leading to the page; empty for the top.
   * @returns the section a tab moved to, which replaces this one; undefined after Escape.
   */
  private async page(keys: readonly string[], copy: TuiCopy, interactions: Interactions, signal: AbortSignal,
    session: PanelSession, progress: Progress): Promise<string | undefined> {
    let initial: string | undefined
    for (;;) {
      // Read again each time, so a row shows what the last pick saved.
      const top = this.sections(copy, session, interactions)
      const trail = locate(top, keys)
      if (trail === undefined) return undefined
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
      if (listed.length === 0) return undefined
      const tabs = keys.length > 1 ? undefined : {
        items: top.map(section => ({ value: `${TAB}${section.key}`, label: section.label })),
        ...keys[0] === undefined ? {} : { active: `${TAB}${keys[0]}` },
      }
      const picked = await interactions.choose({
        title, choices, initial: initial !== undefined && listed.some(choice => choice.value === initial) ? initial : listed[0]!.value,
        ...warnings.length === 0 ? {} : { warning: warnings.join(' · ') },
        ...tabs === undefined ? {} : { tabs },
      }, signal)
      signal.throwIfAborted()
      if (picked === undefined) return undefined
      progress.problem = undefined
      // A section's tab replaces it; the top page opens the one it names.
      if (picked.startsWith(TAB) && keys.length > 0) return picked.slice(TAB.length)
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
      if (picked.startsWith(SECTION) || picked.startsWith(TAB)) {
        let next: string | undefined = picked.slice(picked.startsWith(TAB) ? TAB.length : SECTION.length)
        while (next !== undefined) {
          // Escape returns to the top on the row of the section it left.
          initial = `${SECTION}${next}`
          next = await this.page([...keys, next], copy, interactions, signal, session, progress)
        }
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
        const again = locate(this.sections(copy, session, interactions), found.length > 1 ? found.slice(0, -1) : keys)
        const now = (found.length > 1 || keys.length > 0 ? again?.at(-1)?.settings : [])?.find(entry => entry.key === setting.key)
        if (now !== undefined && now.shown !== setting.shown) progress.changed = true
      }
    }
  }

  /** Change one setting: flip it, open its picker, or offer its values. */
  private async edit(setting: Setting, copy: TuiCopy, interactions: Interactions, signal: AbortSignal, progress: Progress): Promise<void> {
    if (setting.inFile !== undefined && this.editText !== undefined) {
      let text: string
      try {
        text = await this.editText(`${setting.current}\n`, setting.inFile.name, signal)
      } catch (error) {
        signal.throwIfAborted()
        progress.problem = `${copy.settingsEditorFailed}: ${error instanceof Error ? error.message : String(error)}`
        return
      }
      if (text.trim() !== setting.current.trim()) await save(copy, progress, () => setting.set(text))
      return
    }
    if (setting.inFile !== undefined) {
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
  private sections(copy: TuiCopy, session: PanelSession, interactions: Interactions): readonly Section[] {
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
      ]).concat(this.subagentModelRows(copy, descriptors, session, interactions)), true),
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
      ...this.advanced(copy, descriptors, session, interactions),
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
    // No provider is the default, so the row can read no model until one is saved.
    const selected = defaults.currentSelection()
    const route = selected === undefined ? '' : `${selected.provider}/${selected.model}`
    return [{
      key: 'defaultModel', label: copy.settingsModel, current: route, options: [],
      shown: selected === undefined ? copy.noModel
        : selected.reasoningEffort === undefined ? route : `${route} (${String(selected.reasoningEffort)})`,
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

  /**
   * Whether subagents may pick a model, the models they may pick, and, once
   * they may, the task router that picks among them. Without a model catalog
   * the list stays in the settings file, and the switch is the owner's plain
   * field.
   */
  private subagentModelRows(copy: TuiCopy, descriptors: readonly SettingsDescriptor[], session: PanelSession,
    interactions: Interactions): readonly Setting[] {
    const [toggle] = this.curated({ ns: SUBAGENT_MODELS, path: ['enabled'], label: copy.settingsSubagentModels,
      status: copy.settingsNewSessions }, descriptors, copy)
    const selection = subagentSelection(descriptors)
    if (toggle === undefined || selection === undefined) return toggle === undefined ? [] : [toggle]
    // The router chooses among the allowed models, so it is offered once there are some to choose among.
    const routing = selection.enabled && selection.allowedModels.length > 0 ? this.routerRows(copy, descriptors, session, interactions) : []
    const list = session.listModels
    if (list === undefined) return [toggle, ...routing]
    const choose = (enable: boolean) => (signal: AbortSignal): Promise<CommandResult> =>
      this.chooseAllowed(copy, interactions, list, signal, enable)
    const user = descriptors.find(entry => entry.ns === SUBAGENT_MODELS)?.user
    const service = this.service!
    return [
      // Switched on from an empty list, it asks for the models first.
      selection.enabled || selection.allowedModels.length > 0 ? toggle : { ...toggle, open: choose(true) },
      {
        key: `${SUBAGENT_MODELS}.allowedModels`, label: copy.settingsSubagentAllowed, current: '', options: [],
        shown: selection.allowedModels.length === 0 ? copy.settingsSubagentAllowedNone : selection.allowedModels.map(routeText).join(', '),
        status: copy.settingsNewSessions, open: choose(false), set: () => Promise.resolve(),
        ...typeof user === 'object' && user !== null && Object.hasOwn(user, 'allowedModels')
          ? { reset: () => service.mutate(SUBAGENT_MODELS, [{ op: 'unset', path: ['allowedModels'] }]) } : {},
      },
      ...routing,
    ]
  }

  /**
   * The task router, a beta: its switch, which says what it sends before it
   * turns on, its URL, the trade-off it applies, and the list of how it sees
   * each allowed model, where each model's hints are set.
   */
  private routerRows(copy: TuiCopy, descriptors: readonly SettingsDescriptor[], session: PanelSession,
    interactions: Interactions): readonly Setting[] {
    const [toggle] = this.curated({ ns: SUBAGENT_MODELS, path: ['router', 'enabled'], label: copy.settingsRouter }, descriptors, copy)
    const router = routerSettings(descriptors)
    if (toggle === undefined || router === undefined) return []
    const service = this.service!
    const title = `${copy.settingsTitle} › ${copy.settingsAgent} › ${copy.settingsRouter}`
    const account = session.routerAccount
    const rows: Setting[] = [router.enabled
      ? { ...toggle, about: copy.settingsRouterAbout }
      : { ...toggle, about: copy.settingsRouterAbout, open: signal => this.confirmRouter(copy, interactions, router, account, title, signal) }]
    if (!router.enabled) return rows
    if (account !== undefined) {
      rows.push({
        key: `${SUBAGENT_MODELS}.router.account`, label: copy.settingsRouterAccount, current: '', options: [],
        shown: this.routerAccountText(copy),
        open: signal => this.routerAccountMenu(copy, interactions, account, `${title} › ${copy.settingsRouterAccount}`, signal),
        set: () => Promise.resolve(),
      })
    }
    const INFER = '\u0000infer'
    const priorities: readonly Option[] = [
      { value: INFER, label: copy.settingsRouterPriorityInfer, description: copy.settingsDefault },
      { value: 'quality', label: copy.settingsRouterQualityPriority }, { value: 'cost', label: copy.settingsRouterCostPriority },
      { value: 'speed', label: copy.settingsRouterSpeedPriority }, { value: 'balanced', label: copy.settingsRouterBalanced },
    ]
    const priority = router.priority ?? INFER
    rows.push(
      ...this.curated({ ns: SUBAGENT_MODELS, path: ['router', 'url'], label: copy.settingsRouterUrl }, descriptors, copy),
      {
        key: `${SUBAGENT_MODELS}.router.priority`, label: copy.settingsRouterPriority, current: priority, options: priorities,
        shown: priorities.find(option => option.value === priority)?.label ?? priority,
        set: next => service.mutate(SUBAGENT_MODELS, [next === INFER
          ? { op: 'unset', path: ['router', 'priority'] } : { op: 'set', path: ['router', 'priority'], value: next }]),
      },
    )
    const describe = session.describeRoutes
    if (describe !== undefined) {
      rows.push({
        key: `${SUBAGENT_MODELS}.router.hints`, label: copy.settingsRouterCalibrate, current: '', options: [],
        shown: router.hints.length === 0 ? copy.settingsRouterCalibrateAbout
          : router.hints.map(hint => `${routeText(hint)} (${hintText(hint)})`).join(', '),
        open: signal => this.calibrate(copy, interactions, describe, signal), set: () => Promise.resolve(),
      })
    }
    return rows
  }

  /**
   * Turn the router on once the user has read what it sends, and to where.
   * Without a token it offers to sign in first, since the hosted router
   * refuses requests that carry none.
   */
  private async confirmRouter(copy: TuiCopy, interactions: Interactions, router: RouterView,
    account: RouterAccountControls | undefined, title: string, signal: AbortSignal): Promise<CommandResult> {
    const status = this.routerToken?.status
    const configured = status?.configured ?? nonEmpty(process.env[router.tokenEnv])
    const offerSignIn = !configured && account !== undefined && status?.writable === true
    const warning = [copy.settingsRouterAbout, router.url, configured ? undefined : copy.settingsRouterSignedOut]
      .filter(part => part !== undefined).join(' · ')
    const picked = await interactions.choose({
      title, warning, initial: offerSignIn ? 'sign-in' : 'on',
      choices: [
        ...offerSignIn ? [{ value: 'sign-in', label: copy.settingsRouterSignInTurnOn }] : [],
        { value: 'on', label: copy.settingsRouterTurnOn }, { value: 'off', label: copy.settingsRouterKeepOff },
      ],
    }, signal)
    signal.throwIfAborted()
    if (picked === 'sign-in') {
      const signed = await this.routerSignIn(copy, interactions, account!, `${title} › ${copy.settingsRouterAccount}`, signal)
      if (signed.kind === 'error' || this.routerToken?.status.configured !== true) return signed
    } else if (picked !== 'on') return { kind: 'success' }
    try {
      await this.service!.mutate(SUBAGENT_MODELS, [{ op: 'set', path: ['router', 'enabled'], value: true }])
    } catch (error) { return { kind: 'error', text: failure(copy, error) } }
    return { kind: 'success' }
  }

  /** The account row: who is signed in, or where the token comes from. */
  private routerAccountText(copy: TuiCopy): string {
    const state = this.routerToken
    if (state === undefined || !state.status.configured) return copy.settingsRouterSignedOut
    if (!state.status.writable) return `${copy.settingsRouterTokenFrom} ${state.status.tokenEnv}`
    return state.email === undefined ? copy.settingsRouterSignedIn : `${copy.settingsRouterSignedInAs} ${state.email}`
  }

  /** Sign in when signed out; otherwise show the account, and offer another sign-in or signing out. */
  private async routerAccountMenu(copy: TuiCopy, interactions: Interactions, account: RouterAccountControls, title: string,
    signal: AbortSignal): Promise<CommandResult> {
    let status: RouterTokenView
    try { status = await account.status() } catch (error) { return { kind: 'error', text: failure(copy, error) } }
    this.routerToken = { status }
    if (!status.configured) return this.routerSignIn(copy, interactions, account, title, signal)
    if (!status.writable) return { kind: 'error', text: `${copy.settingsRouterTokenFrom} ${status.tokenEnv}` }
    let email: string | undefined
    let problem: string | undefined
    try {
      email = await account.account(signal)
    } catch (error) {
      signal.throwIfAborted()
      problem = `${copy.settingsRouterUnavailable}: ${error instanceof Error ? error.message : String(error)}`
    }
    this.routerToken = { status, ...email === undefined ? {} : { email } }
    const picked = await interactions.choose({
      title, initial: 'keep', ...problem === undefined ? {} : { warning: problem },
      choices: [
        { value: 'keep', label: email === undefined ? copy.settingsRouterSignedIn : `${copy.settingsRouterSignedInAs} ${email}` },
        { value: 'switch', label: copy.settingsRouterSwitch },
        { value: 'out', label: copy.settingsRouterSignOut },
      ],
    }, signal)
    signal.throwIfAborted()
    if (picked === 'switch') return this.routerSignIn(copy, interactions, account, title, signal)
    if (picked !== 'out') return { kind: 'success' }
    try {
      await account.signOut(signal)
      this.routerToken = { status: await account.status() }
    } catch (error) {
      signal.throwIfAborted()
      return { kind: 'error', text: failure(copy, error) }
    }
    return { kind: 'success' }
  }

  /**
   * Ask for an email, have the router send it a code, and trade the code for
   * a token the plugin stores. A refusal is shown on the field and asked
   * again; Escape leaves the stored token as it was.
   */
  private async routerSignIn(copy: TuiCopy, interactions: Interactions, account: RouterAccountControls, title: string,
    signal: AbortSignal): Promise<CommandResult> {
    let status: RouterTokenView
    try { status = await account.status() } catch (error) { return { kind: 'error', text: failure(copy, error) } }
    if (!status.writable) {
      return { kind: 'error', text: status.configured
        ? `${copy.settingsRouterTokenFrom} ${status.tokenEnv}` : `${copy.settingsRouterNoStore} ${status.tokenEnv}` }
    }
    const ask = async (field: Omit<LoginPrompt & { kind: 'text' }, 'kind' | 'title' | 'signal'>): Promise<string | undefined> => {
      try {
        return (await interactions.prompt({ kind: 'text', title, ...field }, signal)).trim()
      } catch (error) {
        signal.throwIfAborted()
        if (error instanceof Error && error.message === 'Authorization cancelled') return undefined
        throw error
      }
    }
    let email = ''
    let refused: string | undefined
    for (;;) {
      const typed = await ask({ message: copy.settingsRouterEmail, placeholder: 'you@example.com', hint: copy.settingsRouterEmailHint,
        step: { index: 1, count: 2 }, ...email === '' ? {} : { initial: email }, ...refused === undefined ? {} : { error: refused } })
      if (typed === undefined) return { kind: 'success' }
      email = typed
      try {
        await account.requestCode(email, signal)
        break
      } catch (error) {
        signal.throwIfAborted()
        refused = error instanceof Error ? error.message : String(error)
      }
    }
    refused = undefined
    for (;;) {
      const code = await ask({ message: `${copy.settingsRouterCode} ${email}`, placeholder: '123456', hint: copy.settingsRouterCodeHint,
        step: { index: 2, count: 2 }, ...refused === undefined ? {} : { error: refused } })
      if (code === undefined) return { kind: 'success' }
      try {
        const signed = await account.signIn(email, code, signal)
        this.routerToken = { status: await account.status(), email: signed }
        return { kind: 'success' }
      } catch (error) {
        signal.throwIfAborted()
        refused = error instanceof Error ? error.message : String(error)
      }
    }
  }

  /**
   * List how the router sees each allowed model, asking it again after every
   * change, and edit the hints of the model Enter picks, until Escape or Done.
   */
  private async calibrate(copy: TuiCopy, interactions: Interactions,
    describe: (signal: AbortSignal) => Promise<readonly RouteView[]>, signal: AbortSignal): Promise<CommandResult> {
    const title = `${copy.settingsTitle} › ${copy.settingsAgent} › ${copy.settingsRouterCalibrate}`
    let initial: string | undefined
    for (;;) {
      const router = routerSettings(this.service?.describe() ?? [])
      if (router === undefined) return { kind: 'success' }
      let views: readonly RouteView[]
      try {
        views = await describe(signal)
      } catch (error) {
        signal.throwIfAborted()
        return { kind: 'error', text: `${copy.settingsRouterUnavailable}: ${error instanceof Error ? error.message : String(error)}` }
      }
      signal.throwIfAborted()
      const hints = new Map(router.hints.map(hint => [routeText(hint), hint]))
      const choices: Choice[] = [
        ...views.map((view) => {
          const hint = hints.get(routeText(view))
          const description = [view.profile ?? copy.settingsRouterUnknown,
            view.quality === undefined ? undefined : `${copy.settingsRouterQualityShort} ${view.quality.toFixed(2)}`,
            view.price === undefined ? undefined : `$${view.price.toFixed(2)}/M`,
            hint === undefined || hintText(hint) === '' ? undefined : `${copy.settingsRouterHinted}: ${hintText(hint)}`,
            hint?.quality !== undefined && view.qualitySource === 'benchmarks' ? copy.settingsRouterBenchmarksWin : undefined,
          ].filter(part => part !== undefined).join(' · ')
          return { value: routeText(view), label: routeText(view), description, status: view.ranked
            ? { text: copy.settingsRouterRanked, tone: 'done' as const } : { text: copy.settingsRouterUnranked, tone: 'waiting' as const } }
        }),
        { value: DONE, label: copy.settingsSubagentAllowedDone, pinned: true },
      ]
      const picked = await interactions.choose({
        title, choices, initial: initial ?? choices[0]!.value,
        ...views.length > 0 && views.every(view => !view.ranked) ? { warning: copy.settingsRouterNoneRanked } : {},
      }, signal)
      signal.throwIfAborted()
      if (picked === undefined || picked === DONE) return { kind: 'success' }
      initial = picked
      const view = views.find(entry => routeText(entry) === picked)!
      const next = await this.editHint(copy, interactions, `${title} › ${picked}`, hints.get(picked) ?? { provider: view.provider, model: view.model }, signal)
      if (next === undefined) continue
      const kept = router.hints.filter(hint => routeText(hint) !== picked)
      const declared = next.sameAs !== undefined || next.quality !== undefined || next.cost !== undefined
      try {
        await this.service!.mutate(SUBAGENT_MODELS, [{ op: 'set', path: ['router', 'hints'], value: [...kept, ...declared ? [next] : []] }])
      } catch (error) { return { kind: 'error', text: failure(copy, error) } }
    }
  }

  /**
   * Change one hint of a model: its quality tier, its cost tier, or the
   * benchmarked model it serves.
   * @returns the model's hints after the change, or undefined when nothing changed.
   */
  private async editHint(copy: TuiCopy, interactions: Interactions, title: string, hint: RouteHint,
    signal: AbortSignal): Promise<RouteHint | undefined> {
    const AUTO = '\u0000auto'
    const tiers: Readonly<Record<string, string>> = {
      free: copy.settingsTierFree, low: copy.settingsTierLow, medium: copy.settingsTierMedium,
      high: copy.settingsTierHigh, frontier: copy.settingsTierFrontier,
    }
    const field = await interactions.choose({
      title, initial: 'quality',
      choices: [
        { value: 'quality', label: copy.settingsRouterQuality, description: hint.quality === undefined ? copy.settingsRouterAuto : tiers[hint.quality] ?? hint.quality },
        { value: 'cost', label: copy.settingsRouterCost, description: hint.cost === undefined ? copy.settingsRouterAuto : tiers[hint.cost] ?? hint.cost },
        { value: 'sameAs', label: copy.settingsRouterSameAs, description: hint.sameAs ?? copy.settingsRouterNone },
      ],
    }, signal)
    signal.throwIfAborted()
    if (field === 'sameAs') {
      let text: string
      try {
        text = await interactions.prompt({ kind: 'text', message: `${copy.settingsRouterSameAs} · ${copy.settingsRouterSameAsPrompt}` }, signal)
      } catch (error) {
        // Escape leaves the hint as it was.
        if (signal.aborted) throw error
        return undefined
      }
      const { sameAs: _, ...rest } = hint
      return text.trim() === '' ? rest : { ...rest, sameAs: text.trim() }
    }
    if (field !== 'quality' && field !== 'cost') return undefined
    const levels = field === 'quality' ? ['low', 'medium', 'high', 'frontier'] : ['free', 'low', 'medium', 'high']
    const current = hint[field] ?? AUTO
    const picked = await interactions.choose({
      title: `${title} › ${field === 'quality' ? copy.settingsRouterQuality : copy.settingsRouterCost}`, initial: current,
      choices: [AUTO, ...levels].map(value => ({ value, label: value === AUTO ? copy.settingsRouterAuto : tiers[value]!,
        ...value === current ? { current: true } : {} })),
    }, signal)
    signal.throwIfAborted()
    if (picked === undefined || picked === current) return undefined
    const { [field]: _, ...rest } = hint
    return picked === AUTO ? rest : { ...rest, [field]: picked }
  }

  /**
   * Add and remove the models subagents may pick, one Enter at a time, each
   * saved as it is made, until Escape or Done. The last model of a switched-on
   * choice is kept, since the owner refuses an empty list.
   * @param enable - switch the choice on with the first model added.
   */
  private async chooseAllowed(copy: TuiCopy, interactions: Interactions,
    list: (signal: AbortSignal) => Promise<ModelCatalog | undefined>, signal: AbortSignal, enable: boolean): Promise<CommandResult> {
    const catalog = await list(signal)
    signal.throwIfAborted()
    if (catalog === undefined) return { kind: 'error', text: copy.noModelSelection }
    let enabling = enable
    let warning: string | undefined = enable ? copy.settingsSubagentAllowedFirst : undefined
    let initial: string | undefined
    for (;;) {
      const selection = subagentSelection(this.service?.describe() ?? [])
      if (selection === undefined) return { kind: 'success' }
      const allowed = new Set(selection.allowedModels.map(routeText))
      // A stored model a catalog no longer lists stays, so it can be removed.
      const routes = new Map<string, { readonly route: ModelRoute, readonly name?: string }>()
      for (const route of selection.allowedModels) routes.set(routeText(route), { route })
      for (const entry of catalog.entries) {
        const route = parseRoute(entry.route)
        if (route !== undefined) routes.set(entry.route, { route, ...namesRoute(entry.name, entry.route) ? {} : { name: entry.name } })
      }
      const choices: Choice[] = [
        ...[...routes].map(([value, { name }]) => ({ value, label: value, ...name === undefined ? {} : { description: name },
          ...allowed.has(value) ? { status: { text: copy.settingsSubagentAllowedOn, tone: 'done' as const } } : {} })),
        { value: DONE, label: copy.settingsSubagentAllowedDone, pinned: true },
      ]
      const warnings = [warning, catalog.unavailable.length === 0 ? undefined : `${copy.modelCatalogError}: ${catalog.unavailable.join(', ')}`]
        .filter(text => text !== undefined)
      const picked = await interactions.choose({
        title: `${copy.settingsTitle} › ${copy.settingsAgent} › ${copy.settingsSubagentAllowed}`, choices,
        initial: initial ?? choices.find(choice => allowed.has(choice.value))?.value ?? choices[0]!.value,
        ...warnings.length === 0 ? {} : { warning: warnings.join(' · ') },
      }, signal)
      signal.throwIfAborted()
      if (picked === undefined || picked === DONE) return { kind: 'success' }
      initial = picked
      warning = undefined
      const next = allowed.has(picked)
        ? selection.allowedModels.filter(route => routeText(route) !== picked)
        : [...selection.allowedModels, routes.get(picked)!.route]
      if (next.length === 0 && selection.enabled) { warning = copy.settingsSubagentAllowedLast; continue }
      try {
        await this.service!.mutate(SUBAGENT_MODELS, [
          { op: 'set', path: ['allowedModels'], value: next.map(route => ({ provider: route.provider, model: route.model })) },
          ...enabling ? [{ op: 'set' as const, path: ['enabled'], value: true }] : [],
        ])
        enabling = false
      } catch (error) { return { kind: 'error', text: failure(copy, error) } }
    }
  }

  /** Advanced: schema fields, sharing the Agent section's model and router pickers. */
  private advanced(copy: TuiCopy, descriptors: readonly SettingsDescriptor[], session: PanelSession,
    interactions: Interactions): readonly Section[] {
    const modelRows = new Map(this.subagentModelRows(copy, descriptors, session, interactions)
      .map(row => [row.key, row]))
    const sections = descriptors
      .filter(descriptor => descriptor.ns !== SETTINGS_NAMESPACE)
      .map(descriptor => {
        const restart = descriptor.applies === 'restart'
        const settings = schemaFields(descriptor.schema, descriptor.value, descriptor.user)
          .map(field => {
            const key = field.path.join('.')
            const row = modelRows.get(`${descriptor.ns}.${key}`)
            return row === undefined ? this.fieldSetting(descriptor.ns, field, copy, restart)
              : { ...row, key, label: key }
          })
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
        ...common, current: JSON.stringify(field.value ?? null, null, 2), options: [], inFile: { name: `${ns}.${key}.json` },
        shown: `${preview(field.value)} · ${this.editText === undefined ? copy.settingsInFile : copy.settingsInEditor}`,
        set: text => {
          let node: ReturnType<typeof parseJsonc>
          try { node = parseJsonc(text) } catch (error) {
            return Promise.reject(new Error(`${copy.settingsNotJson}: ${error instanceof Error ? error.message : String(error)}`))
          }
          return node === undefined ? Promise.reject(new Error(copy.settingsNotJson)) : write(valueOf(node))
        },
      }
    }
  }
}

/** The subagent model choice as its owner resolved it, when its namespace is registered. */
function subagentSelection(descriptors: readonly SettingsDescriptor[]): { readonly enabled: boolean, readonly allowedModels: readonly ModelRoute[] } | undefined {
  const value = descriptors.find(entry => entry.ns === SUBAGENT_MODELS)?.value
  if (typeof value !== 'object' || value === null) return undefined
  const { enabled, allowedModels } = value as { enabled?: unknown, allowedModels?: unknown }
  if (typeof enabled !== 'boolean' || !Array.isArray(allowedModels)) return undefined
  return { enabled, allowedModels: allowedModels.filter((route): route is ModelRoute => typeof route === 'object' && route !== null
    && typeof (route as ModelRoute).provider === 'string' && typeof (route as ModelRoute).model === 'string') }
}

/** The task router's settings as its owner resolved them, when its namespace is registered. */
function routerSettings(descriptors: readonly SettingsDescriptor[]): RouterView | undefined {
  const value = descriptors.find(entry => entry.ns === SUBAGENT_MODELS)?.value as { router?: unknown } | undefined
  const router = value?.router as Partial<Record<keyof RouterView, unknown>> | undefined
  if (typeof router !== 'object' || router === null || typeof router.enabled !== 'boolean' || typeof router.url !== 'string') return undefined
  const hints = Array.isArray(router.hints) ? router.hints.filter((hint): hint is RouteHint => typeof hint === 'object' && hint !== null
    && typeof (hint as RouteHint).provider === 'string' && typeof (hint as RouteHint).model === 'string') : []
  return {
    enabled: router.enabled, url: router.url, tokenEnv: typeof router.tokenEnv === 'string' ? router.tokenEnv : '',
    ...typeof router.priority === 'string' ? { priority: router.priority } : {}, hints,
  }
}

/** A model's declared hints, compactly: quality, cost, then the model it serves. */
const nonEmpty = (value: string | undefined): boolean => value !== undefined && value.length > 0

const hintText = (hint: RouteHint): string => [hint.quality, hint.cost, hint.sameAs].filter(part => part !== undefined).join(' · ')

/** A route as `/model` writes it. */
const routeText = (route: ModelRoute): string => `${route.provider}/${route.model}`

/** A catalog route's provider and model. Provider ids hold no slash; model ids may. */
function parseRoute(text: string): ModelRoute | undefined {
  const separator = text.indexOf('/')
  return separator <= 0 || separator === text.length - 1 ? undefined : { provider: text.slice(0, separator), model: text.slice(separator + 1) }
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
