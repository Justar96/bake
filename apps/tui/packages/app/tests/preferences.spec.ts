/**
 * The `tui` settings namespace and the `/settings` panel: a pick is saved to
 * the settings document, screen and language wait for the next launch, a flag
 * outranks the user's screen, and without a document the change stays local.
 * Plugin settings are offered in named sections and, from their schemas, under
 * Advanced, and the top page searches all of them.
 */
import { afterEach, describe, expect, it, vi } from 'vitest'
import { Context } from '@deepseek-ai/cordis'
import z from '@deepseek-ai/schemastery'
import { SettingsProvider, type SettingsNamespace } from '@deepseek-ai/dsh-settings'
import Defaults from '@deepseek-ai/dsh-agent-default-model'
import { ReasoningEffortId } from '@deepseek-ai/dsh-llm'
import { dictionaries } from '@dsh-tui/ui/copy.ts'
import type { ChoicePrompt } from '@dsh-tui/ui/picker.tsx'
import type { Interactions } from '../src/interactions.ts'
import { Preferences, SETTINGS_NAMESPACE, type TuiSettings } from '../src/preferences.ts'
import { formatBytes, formatDuration, parseNumber, schemaFields } from '../src/schema-fields.ts'

const copy = dictionaries.en
const base: TuiSettings = {
  screen: 'inline', locale: 'en', composerFrame: 'auto', goalObjective: false,
  resultLines: 4, completionLimit: 8, doubleInterruptMs: 2000,
}

/** A settings store over one in-memory document. */
class MemoryProvider extends SettingsProvider {
  static doc: Record<string, unknown> = {}
  readonly writable = true
  protected load(): Promise<Record<string, unknown>> { return Promise.resolve(structuredClone(MemoryProvider.doc)) }
  protected persist(ns: SettingsNamespace, section: Record<string, unknown>): Promise<void> {
    MemoryProvider.doc[ns] = structuredClone(section)
    return Promise.resolve()
  }
}

const disposers: (() => Promise<unknown>)[] = []
afterEach(async () => {
  for (const dispose of disposers.splice(0)) await dispose()
  MemoryProvider.doc = {}
})

async function mount(options: { readonly settings?: boolean, readonly screen?: 'inline' | 'fullscreen' } = {}) {
  const ctx = new Context()
  disposers.push(() => ctx.fiber.dispose())
  if (options.settings !== false) await ctx.plugin(MemoryProvider)
  const changed = vi.fn()
  const preferences = new Preferences(ctx, base, options.screen, changed)
  if (options.settings !== false) await vi.waitFor(() => expect(ctx.settings.get(SETTINGS_NAMESPACE)).toBeDefined())
  return { ctx, preferences, changed }
}

/** One scripted answer: a picker's choice, or typed text for a prompt. */
type Answer = ((prompt: ChoicePrompt) => string | undefined) | { readonly typed: string } | { readonly cancel: true }

/**
 * A picker queue that answers from a script. Each answer reads the prompt it
 * was given, so a test can assert what the panel showed.
 */
function scripted(...answers: Answer[]) {
  const prompts: ChoicePrompt[] = []
  const typed: { kind: string, message: string }[] = []
  const next = (): Answer => {
    const answer = answers.shift()
    if (answer === undefined) throw new Error('unexpected prompt')
    return answer
  }
  const interactions = {
    choose: (prompt: ChoicePrompt) => {
      prompts.push(prompt)
      const answer = next()
      if (typeof answer !== 'function') throw new Error(`expected a picker, got ${JSON.stringify(answer)} for ${prompt.title}`)
      return Promise.resolve(answer(prompt))
    },
    prompt: (prompt: { kind: string, message: string }) => {
      typed.push(prompt)
      const answer = next()
      if (typeof answer === 'function') throw new Error(`expected text for ${prompt.message}`)
      return 'typed' in answer ? Promise.resolve(answer.typed) : Promise.reject(new Error('Authorization cancelled'))
    },
  } as unknown as Interactions
  return { interactions, prompts, typed }
}

/** Pick a value outright. */
const pick = (value: string) => () => value
/** Leave the page. */
const back = () => undefined
/** Pick the value whose label is `label`. */
const labelled = (label: string) => (prompt: ChoicePrompt) => prompt.choices.find(choice => choice.label === label)?.value
const run = (preferences: Preferences, interactions: Interactions, session = {}) =>
  preferences.panel(copy, interactions, new AbortController().signal, session)

/** The shell and agent-loop sections as their plugins register them. */
function plugins(ctx: Context) {
  ctx.settings.register('shell', z.object({
    cwd: z.string(),
    timeoutMs: z.number().default(120_000),
    maxTimeoutMs: z.number().default(600_000),
    maxOutputBytes: z.number().default(64_000),
  }), { validate: value => { if (value.timeoutMs > value.maxTimeoutMs) throw new Error('timeoutMs exceeds maxTimeoutMs') } })
  ctx.settings.register('agent-loop', z.object({ maxParallelToolCalls: z.number().step(1).min(1).default(4) }))
}

describe('Preferences', () => {
  it('saves a toggle and a choice, and holds the screen until the next launch', async () => {
    const { preferences, changed } = await mount()
    expect(preferences.launch.screen).toBe('inline')
    const { interactions, prompts } = scripted(
      pick('section:terminal'),
      pick('setting:goalObjective'),
      pick('setting:screen'), pick('fullscreen'),
      back, back,
    )
    await expect(run(preferences, interactions)).resolves.toEqual({ kind: 'success', text: copy.settingsStored })
    expect(MemoryProvider.doc[SETTINGS_NAMESPACE]).toEqual({ goalObjective: true, screen: 'fullscreen' })
    expect(preferences.value).toMatchObject({ goalObjective: true, screen: 'fullscreen' })
    await vi.waitFor(() => expect(changed).toHaveBeenCalled())
    // The process keeps the screen it started with; the panel says when the change lands.
    expect(preferences.screen).toBe('inline')
    const page = prompts.at(-2)!
    expect(page.title).toBe(`${copy.settingsTitle} › ${copy.settingsTerminal}`)
    expect(page.initial).toBe('setting:screen')
    expect(page.choices.find(choice => choice.value === 'setting:screen')).toMatchObject({
      description: copy.settingsScreenFullscreen, status: { text: copy.settingsNextLaunch },
    })
    expect(page.choices.find(choice => choice.value === 'setting:goalObjective')?.description).toBe(copy.settingsOn)
    // The value picker marks what is in force, and the profile's value.
    expect(prompts[3]!.choices.find(choice => choice.current === true)?.value).toBe('inline')
    expect(prompts[3]!.choices.find(choice => choice.value === 'inline')?.description).toBe(copy.settingsDefault)
    // Back at the top, the section's row reads its values.
    expect(prompts.at(-1)!.choices.find(choice => choice.value === 'section:terminal')?.description)
      .toBe(`${copy.settingsScreenFullscreen} · English · ${copy.settingsFrameAuto}`)
  })

  it('closes without a message when nothing changed, and offers no reset over defaults', async () => {
    const { preferences } = await mount()
    const { interactions, prompts } = scripted(pick('section:terminal'), pick('setting:locale'), back, back, back)
    await expect(run(preferences, interactions)).resolves.toEqual({ kind: 'success' })
    expect(prompts[1]!.choices.some(choice => choice.pinned === true)).toBe(false)
    expect(prompts[2]!.choices.some(choice => choice.pinned === true)).toBe(false)
  })

  it('resets a section after a confirmation, keeps it on a refusal, and resets one setting on its own', async () => {
    MemoryProvider.doc = { [SETTINGS_NAMESPACE]: { resultLines: 16, locale: 'zh', completionLimit: 12 }, other: { kept: true } }
    const { preferences } = await mount()
    const reset = (prompt: ChoicePrompt) => prompt.choices.find(choice => choice.pinned === true)?.value
    const { interactions, prompts } = scripted(
      pick('section:terminal'),
      // One setting: its value list ends with its own reset.
      pick('setting:completionLimit'), reset,
      reset, pick('keep'),
      reset, pick('reset'),
      back, back,
    )
    await run(preferences, interactions)
    expect(prompts[2]!.choices.at(-1)).toMatchObject({ label: copy.settingsResetField, pinned: true })
    expect(prompts[4]!.title).toBe(copy.settingsResetConfirm)
    expect(prompts[4]!.initial).toBe('keep')
    expect(MemoryProvider.doc[SETTINGS_NAMESPACE]).toEqual({})
    expect(MemoryProvider.doc['other']).toEqual({ kept: true })
    expect(preferences.value).toEqual(base)
    // Nothing left to reset.
    expect(prompts.at(-2)!.choices.some(choice => choice.pinned === true)).toBe(false)
  })

  it('keeps the panel open with the reason when a save fails', async () => {
    const { preferences } = await mount()
    const persist = vi.spyOn(MemoryProvider.prototype as unknown as { persist: () => Promise<void> }, 'persist')
      .mockRejectedValueOnce(new Error('disk full'))
    const { interactions, prompts } = scripted(pick('section:terminal'),
      pick('setting:resultLines'), pick('16'), pick('setting:resultLines'), pick('8'), back, back)
    await expect(run(preferences, interactions)).resolves.toMatchObject({ kind: 'success' })
    expect(prompts[3]!.warning).toBe(`${copy.settingsFailed}: disk full`)
    expect(prompts[5]!.warning).toBeUndefined()
    expect(MemoryProvider.doc[SETTINGS_NAMESPACE]).toEqual({ resultLines: 8 })
    persist.mockRestore()
  })

  it('takes a typed number within its bounds, and names the bound a typed number breaks', async () => {
    const { preferences } = await mount()
    const { interactions, prompts, typed } = scripted(pick('section:terminal'),
      pick('setting:resultLines'), labelled(copy.settingsCustom), { typed: '-1' },
      pick('setting:resultLines'), labelled(copy.settingsCustom), { typed: 'lots' },
      pick('setting:resultLines'), labelled(copy.settingsCustom), { cancel: true },
      pick('setting:doubleInterruptMs'), labelled(copy.settingsCustom), { typed: '1.5s' },
      pick('setting:resultLines'), labelled(copy.settingsCustom), { typed: '12' },
      back, back)
    await run(preferences, interactions)
    expect(prompts[3]!.warning).toBe(`${copy.settingsResultLines}: ${copy.settingsBelowMin} 0`)
    expect(prompts[5]!.warning).toBe(`${copy.settingsResultLines}: ${copy.settingsNotNumber}`)
    // Escape from the text leaves the value and the page as they were.
    expect(prompts[7]!.warning).toBeUndefined()
    expect(typed[3]).toEqual({ kind: 'text', message: `${copy.settingsDoubleInterrupt} · ${copy.settingsDurationHint}` })
    expect(MemoryProvider.doc[SETTINGS_NAMESPACE]).toEqual({ doubleInterruptMs: 1500, resultLines: 12 })
    expect(prompts.at(-2)!.choices.find(choice => choice.value === 'setting:doubleInterruptMs')?.description).toBe('1500ms')
  })

  it('offers the default model through the session\'s model picker', async () => {
    const { ctx, preferences } = await mount()
    await ctx.plugin(Defaults, { provider: 'mock', model: 'model' })
    await vi.waitFor(() => expect(ctx.settings.get('agent-default-model')).toBeDefined())
    const chooseModel = vi.fn(async () => {
      await ctx.agentDefaultModel.saveSelection({ provider: 'mock', model: 'other', reasoningEffort: ReasoningEffortId('high') })
      return { kind: 'success' as const }
    })
    const { interactions, prompts } = scripted(pick('section:session'), pick('setting:defaultModel'), back, back)
    const result = await run(preferences, interactions, { chooseModel })
    expect(chooseModel).toHaveBeenCalledOnce()
    expect(prompts[0]!.choices.find(choice => choice.searchOnly !== true)).toMatchObject({ value: 'section:session', label: copy.settingsSession, description: 'mock/model' })
    expect(prompts[1]!.choices[0]).toMatchObject({ value: 'setting:defaultModel', description: 'mock/model', status: { text: copy.settingsNewSessions } })
    expect(prompts[2]!.choices[0]).toMatchObject({ value: 'setting:defaultModel', description: 'mock/other (high)' })
    expect(result).toEqual({ kind: 'success', text: copy.settingsStored })
    expect(MemoryProvider.doc['agent-default-model']).toEqual({ provider: 'mock', model: 'other', reasoningEffort: 'high' })
  })

  it('lets a --screen flag outrank the user\'s screen', async () => {
    MemoryProvider.doc = { [SETTINGS_NAMESPACE]: { screen: 'fullscreen' } }
    const { preferences } = await mount({ screen: 'inline' })
    expect(preferences.value.screen).toBe('fullscreen')
    expect(preferences.screen).toBe('inline')
    const { interactions, prompts } = scripted(pick('section:terminal'), back, back)
    await run(preferences, interactions)
    expect(prompts[1]!.choices.find(choice => choice.value === 'setting:screen')?.status?.text).toBe(copy.settingsByFlag)
  })

  it('reads a stored choice at launch', async () => {
    MemoryProvider.doc = { [SETTINGS_NAMESPACE]: { screen: 'fullscreen', locale: 'zh' } }
    const { preferences } = await mount()
    expect([preferences.screen, preferences.launch.locale]).toEqual(['fullscreen', 'zh'])
  })

  it('keeps the profile\'s values when the stored section is invalid', async () => {
    MemoryProvider.doc = { [SETTINGS_NAMESPACE]: { screen: 'sideways' } }
    const ctx = new Context()
    disposers.push(() => ctx.fiber.dispose())
    await ctx.plugin(MemoryProvider)
    const warn = vi.spyOn(ctx.logger, 'warn').mockImplementation(() => {})
    const preferences = new Preferences(ctx, base, undefined, vi.fn())
    await vi.waitFor(() => expect(warn).toHaveBeenCalled())
    expect(preferences.value).toEqual(base)
  })

  it('changes only this process without a settings document, and says so', async () => {
    const { preferences, changed } = await mount({ settings: false })
    const { interactions, prompts } = scripted(pick('section:terminal'), pick('setting:resultLines'), pick('16'), back, back)
    await expect(run(preferences, interactions)).resolves.toEqual({ kind: 'success', text: copy.settingsUnsaved })
    expect(preferences.value.resultLines).toBe(16)
    expect(changed).toHaveBeenCalledOnce()
    expect(prompts[0]!.warning).toBe(copy.settingsUnsaved)
    // Only the terminal's own settings exist without a document.
    expect(prompts[0]!.choices.filter(choice => choice.searchOnly !== true).map(choice => choice.value)).toEqual(['section:terminal'])
    // A reset returns to the profile's values without a document too.
    expect(prompts[3]!.choices.some(choice => choice.pinned === true)).toBe(true)
  })

  it('offers the default access preset from its registered namespace', async () => {
    const { ctx, preferences } = await mount()
    ctx.settings.register('permission', z.object({
      defaultPreset: z.union([z.const('read-only').description('Read only'), z.const('workspace-write')]).required(),
    }), { base: { defaultPreset: 'read-only' } })
    const { interactions, prompts } = scripted(pick('section:session'), pick('setting:defaultPreset'), pick('workspace-write'), back, back)
    await run(preferences, interactions)
    expect(prompts[2]!.choices.map(choice => choice.value)).toEqual(['read-only', 'workspace-write'])
    expect(prompts[2]!.choices[0]!.description).toBe('Read only')
    expect(MemoryProvider.doc['permission']).toEqual({ defaultPreset: 'workspace-write' })
  })

  it('names the plugin settings a user reaches for, in their units, and saves each field alone', async () => {
    const { ctx, preferences } = await mount()
    plugins(ctx)
    const { interactions, prompts } = scripted(
      pick('section:shell'),
      pick('setting:shell.timeoutMs'), labelled('5m'),
      pick('setting:shell.maxOutputBytes'), labelled(copy.settingsCustom), { typed: '256KB' },
      // The owner refuses a timeout past the longest one allowed; the panel says so and stays.
      pick('setting:shell.timeoutMs'), labelled(copy.settingsCustom), { typed: '1h' },
      back,
      pick('section:agent'), pick('setting:agent-loop.maxParallelToolCalls'), labelled('8'), back,
      back,
    )
    await run(preferences, interactions)
    const top = prompts[0]!.choices.filter(choice => choice.searchOnly !== true).map(choice => choice.label)
    expect(top).toEqual([copy.settingsTerminal, copy.settingsAgent, copy.settingsShell, copy.settingsAdvanced])
    // A bare number says nothing a page up, so a plugin section names what it holds.
    expect(prompts[0]!.choices.find(choice => choice.value === 'section:shell')?.description)
      .toBe([copy.settingsShellTimeout, copy.settingsShellMaxTimeout, copy.settingsShellOutput].join(' · '))
    expect(prompts[0]!.choices.find(choice => choice.value === 'section:advanced')?.description).toBe(copy.settingsAdvancedAbout)
    expect(prompts[1]!.choices.map(choice => [choice.label, choice.description])).toEqual([
      [copy.settingsShellTimeout, '2m'], [copy.settingsShellMaxTimeout, '10m'], [copy.settingsShellOutput, '64 KB'],
    ])
    // Steps in the setting's unit, the schema's default marked, and a typed value last.
    expect(prompts[2]!.choices.map(choice => choice.label)).toEqual(['30s', '1m', '2m', '5m', '10m', copy.settingsCustom])
    expect(prompts[2]!.choices.find(choice => choice.label === '2m')).toMatchObject({ current: true, description: copy.settingsDefault })
    expect(prompts[8]!.warning).toBe(`${copy.settingsFailed}: timeoutMs exceeds maxTimeoutMs`)
    expect(MemoryProvider.doc['shell']).toEqual({ timeoutMs: 300_000, maxOutputBytes: 256_000 })
    expect(MemoryProvider.doc['agent-loop']).toEqual({ maxParallelToolCalls: 8 })
  })

  it('edits any registered namespace from its schema under Advanced, and never loses a secret it did not show', async () => {
    MemoryProvider.doc = { 'example-plugin': { apiKey: 'kept-secret' } }
    const { ctx, preferences } = await mount()
    ctx.settings.register('example-plugin', z.object({
      apiKey: z.string().role('secret'),
      mode: z.union([z.const('fast').description('Fast answers'), z.const('careful')]).default('fast'),
      verbose: z.boolean().default(false),
      limits: z.object({ retries: z.number().step(1).min(0).max(9).default(2).description('Retries before giving up') }),
      hosts: z.array(z.string()).default([]),
      label: z.string(),
    }), { applies: 'restart' })
    const { interactions, prompts, typed } = scripted(
      pick('section:advanced'), pick('section:example-plugin'),
      pick('setting:verbose'),
      pick('setting:mode'), pick('careful'),
      pick('setting:limits.retries'), labelled(copy.settingsCustom), { typed: '12' },
      pick('setting:limits.retries'), labelled(copy.settingsCustom), { typed: '3' },
      pick('setting:label'), { typed: 'mine' },
      pick('setting:hosts'),
      back, back, back,
    )
    await run(preferences, interactions)
    expect(prompts[1]!.choices.find(choice => choice.value === 'section:example-plugin')).toMatchObject({
      description: `6 ${copy.settingsFields}`, status: { text: copy.settingsNextLaunch, tone: 'waiting' },
    })
    const page = prompts[2]!
    expect(page.title).toBe(`${copy.settingsTitle} › ${copy.settingsAdvanced} › example-plugin`)
    expect(page.choices.filter(choice => choice.pinned !== true).map(choice => [choice.label, choice.description])).toEqual([
      ['apiKey', copy.settingsSecretSet], ['mode', 'Fast answers'], ['verbose', copy.settingsOff],
      ['limits.retries', '2'], ['hosts', `[0] · ${copy.settingsInFile}`], ['label', copy.settingsEmpty],
    ])
    // The owner's description titles the value list.
    expect(prompts[6]!.title).toBe('limits.retries · Retries before giving up')
    expect(prompts[7]!.warning).toBe(`limits.retries: ${copy.settingsAboveMax} 9`)
    // A string with nothing to pick from asks for its text at once.
    expect(typed.at(-1)).toEqual({ kind: 'text', message: 'label' })
    expect(prompts.at(-3)!.warning).toBe(copy.settingsInFile)
    expect(MemoryProvider.doc['example-plugin']).toEqual({
      apiKey: 'kept-secret', verbose: true, mode: 'careful', limits: { retries: 3 }, label: 'mine',
    })
  })

  it('searches every setting from the top page, and edits the one found in place', async () => {
    const { ctx, preferences } = await mount()
    plugins(ctx)
    const { interactions, prompts } = scripted(
      (prompt) => prompt.choices.find(choice => choice.label === `${copy.settingsShell} › ${copy.settingsShellTimeout}`)?.value,
      labelled('1m'),
      back,
    )
    await run(preferences, interactions)
    const found = prompts[0]!.choices.find(choice => choice.label === `${copy.settingsShell} › ${copy.settingsShellTimeout}`)
    expect(found).toMatchObject({ searchOnly: true, description: '2m', value: 'setting:shell/shell.timeoutMs' })
    expect(prompts[0]!.choices.some(choice => choice.label === `${copy.settingsAdvanced} › shell › timeoutMs` && choice.searchOnly === true)).toBe(true)
    expect(MemoryProvider.doc['shell']).toEqual({ timeoutMs: 60_000 })
    // Back at the top with the section of the setting it changed under the pointer.
    expect(prompts[2]!.initial).toBe('section:shell')
  })
})

describe('schemaFields', () => {
  it('flattens nested objects and says how each field is edited and whether the user set it', () => {
    const schema = z.object({
      mode: z.union([z.const('a').description('First'), z.const('b')]),
      count: z.natural().default(3),
      nested: z.object({ on: z.boolean() }),
      key: z.string().role('secret'),
      list: z.array(z.string()),
      either: z.union([z.string(), z.number()]),
    }).toJSON()
    const fields = schemaFields(schema, { mode: 'a', count: 3, nested: { on: true } }, { nested: { on: true } })
    expect(fields.map(field => [field.path.join('.'), field.kind, field.overridden])).toEqual([
      ['mode', 'choice', false], ['count', 'number', false], ['nested.on', 'boolean', true],
      ['key', 'secret', false], ['list', 'other', false], ['either', 'other', false],
    ])
    expect(fields[0]!.choices).toEqual([{ value: 'a', label: 'First' }, { value: 'b', label: 'b' }])
    expect(fields[1]).toMatchObject({ default: 3, min: 0, step: 1, value: 3 })
    expect(schemaFields(undefined, {}, undefined)).toEqual([])
  })
})

describe('typed numbers', () => {
  it('reads units, and refuses what the field does not take', () => {
    expect(parseNumber('90s', {}, 'ms')).toEqual({ value: 90_000 })
    expect(parseNumber('2m', {}, 'ms')).toEqual({ value: 120_000 })
    expect(parseNumber('1.5 MB', {}, 'bytes')).toEqual({ value: 1_500_000 })
    expect(parseNumber('750', {}, 'ms')).toEqual({ value: 750 })
    expect(parseNumber('2m', {})).toEqual({ problem: 'number' })
    expect(parseNumber('0', { min: 1 })).toEqual({ problem: 'min' })
    expect(parseNumber('10', { max: 9 })).toEqual({ problem: 'max' })
    expect(parseNumber('2.5', { step: 1 })).toEqual({ problem: 'step' })
  })

  it('writes durations and sizes the way a reader says them', () => {
    expect([500, 1500, 30_000, 120_000, 3_600_000, 5_400_000].map(formatDuration)).toEqual(['500ms', '1500ms', '30s', '2m', '1h', '1h 30m'])
    expect([512, 64_000, 1_000_000, 1_500_000].map(formatBytes)).toEqual(['512 B', '64 KB', '1 MB', '1.5 MB'])
  })
})
