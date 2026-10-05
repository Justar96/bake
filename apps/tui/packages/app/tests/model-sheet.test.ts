/** The `/model` sheet's order, Recent list, and rows, apart from any catalog. */
import { describe, expect, it } from 'bun:test'
import { ReasoningEffortId } from 'bake-llm'
import { dictionaries } from 'bake-tui-ui/copy.ts'
import { AGENT_TONES, CONTEXT_RAMP, PALETTE } from 'bake-tui-ui/palette.ts'
import { modelSheetPrompt, newestFirst, versionOf, withRecent, RECENT_MODELS, type ModelSheet, type SheetModel } from '../src/model.ts'

const copy = dictionaries.en

describe('versionOf', () => {
  it.each([
    ['GPT-5.5', [5, 5]],
    ['GPT-5.4 mini', [5, 4]],
    ['claude-opus-4-5-20251101', [4, 5]],
    ['GPT-4o (2024-05-13)', [4]],
    ['o3-mini', [3]],
    ['DeepSeek V4 Flash', [4]],
    ['Kimi K2.6', [2, 6]],
    ['llama-3.1-405b', [3, 1]],
  ] as const)('reads %s as %j', (text, version) => {
    expect(versionOf(text)).toEqual([...version])
  })

  it.each(['Daybreak Blue', 'gpt-daybreak-blue-latest', 'qwen3-coder-480b', 'deepseek-flash'])('finds no version in %s', text => {
    expect(versionOf(text)).toBeUndefined()
  })
})

describe('newestFirst', () => {
  const named = (...names: readonly string[]) => names.map(name => ({ name, model: name.toLowerCase().replace(/\s+/g, '-') }))

  it('puts higher versions first and keeps the catalog order of equal ones', () => {
    const models = named('GPT-4', 'GPT-4.1', 'Claude Haiku 4.5 (latest)', 'Claude Haiku 4.5', 'GPT-6.1 Sol', 'GPT-5.5', 'GPT-5')
    expect(newestFirst(models).map(model => model.name)).toEqual(
      ['GPT-6.1 Sol', 'GPT-5.5', 'GPT-5', 'Claude Haiku 4.5 (latest)', 'Claude Haiku 4.5', 'GPT-4.1', 'GPT-4'])
  })

  it('lists models without a version last, by name', () => {
    expect(newestFirst(named('Zeta', 'GPT-5', 'Alpha')).map(model => model.name)).toEqual(['GPT-5', 'Alpha', 'Zeta'])
  })

  it('falls back to the id when the name carries no version', () => {
    expect(newestFirst([{ name: 'Flash', model: 'flash' }, { name: 'Pro', model: 'pro-v4' }]).map(model => model.name))
      .toEqual(['Pro', 'Flash'])
  })
})

describe('withRecent', () => {
  it('moves a chosen route to the front without repeating it, and keeps the newest few', () => {
    expect(withRecent(['a/1', 'b/2', 'c/3'], 'b/2')).toEqual(['b/2', 'a/1', 'c/3'])
    const many = Array.from({ length: RECENT_MODELS }, (_, index) => `p/${index}`)
    expect(withRecent(many, 'p/new')).toEqual(['p/new', ...many.slice(0, RECENT_MODELS - 1)])
  })
})

describe('modelSheetPrompt', () => {
  const efforts = { efforts: [{ id: ReasoningEffortId('low'), name: 'Low' }, { id: ReasoningEffortId('high'), name: 'High' }],
    defaultEffort: ReasoningEffortId('low') }
  const model = (route: string, extra: Partial<SheetModel> = {}): SheetModel => {
    const [provider, id] = route.split('/') as [string, string]
    return { route, provider, model: id, name: id, current: false, ...extra }
  }
  const sheet: ModelSheet = {
    recent: [model('deepseek/flash', { name: 'DeepSeek V4 Flash', current: true, context: 1_000_000, reasoning: efforts })],
    groups: [{ provider: 'openai', name: 'OpenAI', models: [
      model('openai/gpt-5.5', { name: 'GPT-5.5', context: 272_000, image: true, reasoning: efforts }),
      model('openai/gpt-4', { name: 'GPT-4', context: 8192, image: false }),
    ] }],
    providers: [{ id: 'deepseek', name: 'DeepSeek' }, { id: 'openai', name: 'OpenAI' }],
    unavailable: ['offline'],
  }

  it('titles the sheet with its count and providers, and groups its rows', () => {
    const prompt = modelSheetPrompt(sheet, { provider: 'deepseek', model: 'flash' }, copy)
    expect(prompt.title).toBe('Model · 3 from DeepSeek, OpenAI')
    expect(prompt.initial).toBe('deepseek/flash')
    expect(prompt.tall).toBe(true)
    expect(prompt.warning).toBe(`${copy.modelCatalogError}: offline`)
    expect(prompt.choices.map(choice => [choice.group, choice.label])).toEqual([
      [copy.recentModels, 'DeepSeek V4 Flash'], ['OpenAI', 'GPT-5.5'], ['OpenAI', 'GPT-4']])
  })

  it('names the provider on a Recent row and drops a name that only restates the id', () => {
    const [recent, gpt55] = modelSheetPrompt(sheet, { provider: 'deepseek', model: 'flash' }, copy).choices
    expect(recent?.description).toBe('DeepSeek · flash')
    expect(gpt55?.description).toBeUndefined()
  })

  it('draws context, thinking, and image facts', () => {
    const choices = modelSheetPrompt(sheet, { provider: 'deepseek', model: 'flash' }, copy).choices
    expect(choices.map(choice => choice.facts)).toEqual([
      ['1M', copy.factThink, ''], ['272k', copy.factThink, copy.factImage], ['8.2k', '', '']])
  })

  it('opens each model on the session\'s effort when it offers it, and on the default otherwise', () => {
    const choices = modelSheetPrompt(sheet, { provider: 'deepseek', model: 'flash', reasoningEffort: ReasoningEffortId('high') }, copy).choices
    expect(choices[0]?.levels).toEqual({ initial: 'high',
      items: [{ value: '', label: `${copy.effortDefault} (Low)` }, { value: 'low', label: 'Low' },
        { value: 'high', label: 'High', color: PALETTE.asking }] })
    expect(choices[1]?.levels?.initial).toBe('high')
    expect(choices[2]?.levels).toBeUndefined()
    const defaults = modelSheetPrompt(sheet, { provider: 'deepseek', model: 'flash' }, copy).choices
    expect(defaults.map(choice => choice.levels?.initial)).toEqual(['', '', undefined])
  })

  it('draws each effort in its status-line tone, and the default in the tone of the effort it stands for', () => {
    const deep = { efforts: [{ id: ReasoningEffortId('medium'), name: 'Medium' }, { id: ReasoningEffortId('xhigh'), name: 'Extra high' },
      { id: ReasoningEffortId('max'), name: 'Max' }], defaultEffort: ReasoningEffortId('xhigh') }
    const [choice] = modelSheetPrompt({ ...sheet, recent: [model('deepseek/pro', { current: true, reasoning: deep })], groups: [] },
      { provider: 'deepseek', model: 'pro' }, copy).choices
    expect(choice?.levels?.items).toEqual([
      { value: '', label: `${copy.effortDefault} (Extra high)`, color: CONTEXT_RAMP[2] },
      { value: 'medium', label: 'Medium' },
      { value: 'xhigh', label: 'Extra high', color: CONTEXT_RAMP[2] },
      { value: 'max', label: 'Max', color: AGENT_TONES[1] },
    ])
  })
})
