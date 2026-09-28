/**
 * The `tui` settings namespace and the `/settings` panel: a pick is saved to
 * the settings document, screen and language wait for the next launch, a flag
 * outranks the user's screen, and without a document the change stays local.
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
import { constChoices, Preferences, SETTINGS_NAMESPACE, type TuiSettings } from '../src/preferences.ts'

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

/**
 * A picker queue that answers from a script. Each answer reads the prompt it
 * was given, so a test can assert what the panel showed.
 */
function scripted(...answers: ((prompt: ChoicePrompt) => string | undefined)[]) {
  const prompts: ChoicePrompt[] = []
  const interactions = {
    choose: (prompt: ChoicePrompt) => {
      prompts.push(prompt)
      const answer = answers.shift()
      if (answer === undefined) throw new Error('unexpected picker')
      return Promise.resolve(answer(prompt))
    },
  } as unknown as Interactions
  return { interactions, prompts }
}

describe('Preferences', () => {
  it('saves a toggle and a choice, and holds the screen until the next launch', async () => {
    const { preferences, changed } = await mount()
    expect(preferences.launch.screen).toBe('inline')
    const { interactions, prompts } = scripted(
      () => 'goalObjective',
      () => 'screen', () => 'fullscreen',
      () => undefined,
    )
    await expect(preferences.panel(copy, interactions, new AbortController().signal)).resolves.toEqual({ kind: 'success', text: copy.settingsStored })
    expect(MemoryProvider.doc[SETTINGS_NAMESPACE]).toEqual({ goalObjective: true, screen: 'fullscreen' })
    expect(preferences.value).toMatchObject({ goalObjective: true, screen: 'fullscreen' })
    await vi.waitFor(() => expect(changed).toHaveBeenCalled())
    // The process keeps the screen it started with; the panel says when the change lands.
    expect(preferences.screen).toBe('inline')
    const last = prompts.at(-1)!
    expect(last.initial).toBe('screen')
    expect(last.choices.find(choice => choice.value === 'screen')).toMatchObject({
      description: copy.settingsScreenFullscreen, status: { text: copy.settingsNextLaunch },
    })
    expect(last.choices.find(choice => choice.value === 'goalObjective')?.description).toBe(copy.settingsOn)
    // The value picker marks what is in force, and the profile's value.
    expect(prompts[2]!.choices.find(choice => choice.current === true)?.value).toBe('inline')
    expect(prompts[2]!.choices.find(choice => choice.value === 'inline')?.description).toBe(copy.settingsDefault)
  })

  it('closes without a message when nothing changed, and offers no reset over defaults', async () => {
    const { preferences } = await mount()
    const { interactions, prompts } = scripted(() => 'locale', () => undefined, () => undefined)
    await expect(preferences.panel(copy, interactions, new AbortController().signal)).resolves.toEqual({ kind: 'success' })
    expect(prompts[0]!.choices.some(choice => choice.pinned === true)).toBe(false)
  })

  it('resets the tui section after a confirmation, and keeps it on a refusal', async () => {
    MemoryProvider.doc = { [SETTINGS_NAMESPACE]: { resultLines: 16, locale: 'zh' }, other: { kept: true } }
    const { preferences } = await mount()
    const reset = (prompt: ChoicePrompt) => prompt.choices.find(choice => choice.pinned === true)?.value
    const { interactions, prompts } = scripted(
      reset, () => 'keep',
      reset, () => 'reset',
      () => undefined,
    )
    await preferences.panel(copy, interactions, new AbortController().signal)
    expect(prompts[1]!.title).toBe(copy.settingsResetConfirm)
    expect(prompts[1]!.initial).toBe('keep')
    expect(MemoryProvider.doc[SETTINGS_NAMESPACE]).toEqual({})
    expect(MemoryProvider.doc['other']).toEqual({ kept: true })
    expect(preferences.value).toEqual(base)
    // Nothing left to reset.
    expect(prompts.at(-1)!.choices.some(choice => choice.pinned === true)).toBe(false)
  })

  it('keeps the panel open with the reason when a save fails', async () => {
    const { preferences } = await mount()
    const persist = vi.spyOn(MemoryProvider.prototype as unknown as { persist: () => Promise<void> }, 'persist')
      .mockRejectedValueOnce(new Error('disk full'))
    const { interactions, prompts } = scripted(() => 'resultLines', () => '16', () => 'resultLines', () => '8', () => undefined)
    await expect(preferences.panel(copy, interactions, new AbortController().signal)).resolves.toMatchObject({ kind: 'success' })
    expect(prompts[2]!.warning).toBe(`${copy.settingsFailed}: disk full`)
    expect(prompts[4]!.warning).toBeUndefined()
    expect(MemoryProvider.doc[SETTINGS_NAMESPACE]).toEqual({ resultLines: 8 })
    persist.mockRestore()
  })

  it('offers the default model through the session\'s model picker', async () => {
    const { ctx, preferences } = await mount()
    await ctx.plugin(Defaults, { provider: 'mock', model: 'model' })
    await vi.waitFor(() => expect(ctx.settings.get('agent-default-model')).toBeDefined())
    const chooseModel = vi.fn(async () => {
      await ctx.agentDefaultModel.saveSelection({ provider: 'mock', model: 'other', reasoningEffort: ReasoningEffortId('high') })
      return { kind: 'success' as const }
    })
    const { interactions, prompts } = scripted(() => 'defaultModel', () => undefined)
    const result = await preferences.panel(copy, interactions, new AbortController().signal, { chooseModel })
    expect(chooseModel).toHaveBeenCalledOnce()
    expect(prompts[0]!.choices[0]).toMatchObject({ value: 'defaultModel', description: 'mock/model', status: { text: copy.settingsNewSessions } })
    expect(prompts[1]!.choices[0]).toMatchObject({ value: 'defaultModel', description: 'mock/other (high)' })
    expect(result).toEqual({ kind: 'success', text: copy.settingsStored })
    expect(MemoryProvider.doc['agent-default-model']).toEqual({ provider: 'mock', model: 'other', reasoningEffort: 'high' })
  })

  it('lets a --screen flag outrank the user\'s screen', async () => {
    MemoryProvider.doc = { [SETTINGS_NAMESPACE]: { screen: 'fullscreen' } }
    const { preferences } = await mount({ screen: 'inline' })
    expect(preferences.value.screen).toBe('fullscreen')
    expect(preferences.screen).toBe('inline')
    const { interactions, prompts } = scripted(() => undefined)
    await preferences.panel(copy, interactions, new AbortController().signal)
    expect(prompts[0]!.choices.find(choice => choice.value === 'screen')?.status?.text).toBe(copy.settingsByFlag)
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
    const { interactions, prompts } = scripted(() => 'resultLines', () => '16', () => undefined)
    await expect(preferences.panel(copy, interactions, new AbortController().signal)).resolves.toEqual({ kind: 'success', text: copy.settingsUnsaved })
    expect(preferences.value.resultLines).toBe(16)
    expect(changed).toHaveBeenCalledOnce()
    expect(prompts[0]!.warning).toBe(copy.settingsUnsaved)
    // A reset returns to the profile's values without a document too.
    expect(prompts[2]!.choices.some(choice => choice.pinned === true)).toBe(true)
  })

  it('offers the default access preset from its registered namespace', async () => {
    const { ctx, preferences } = await mount()
    ctx.settings.register('permission', z.object({
      defaultPreset: z.union([z.const('read-only').description('Read only'), z.const('workspace-write')]).required(),
    }), { base: { defaultPreset: 'read-only' } })
    const { interactions, prompts } = scripted(() => 'defaultPreset', () => 'workspace-write', () => undefined)
    await preferences.panel(copy, interactions, new AbortController().signal)
    expect(prompts[1]!.choices.map(choice => choice.value)).toEqual(['read-only', 'workspace-write'])
    expect(prompts[1]!.choices[0]!.description).toBe('Read only')
    expect(MemoryProvider.doc['permission']).toEqual({ defaultPreset: 'workspace-write' })
  })
})

describe('constChoices', () => {
  it('reads a union of constants from a serialized schema, and nothing from any other field', () => {
    const schema = z.object({
      mode: z.union([z.const('a').description('First'), z.const('b')]),
      count: z.natural(),
    }).toJSON()
    expect(constChoices(schema, 'mode')).toEqual([{ value: 'a', label: 'First' }, { value: 'b', label: 'b' }])
    expect(constChoices(schema, 'count')).toEqual([])
    expect(constChoices(undefined, 'mode')).toEqual([])
  })
})
