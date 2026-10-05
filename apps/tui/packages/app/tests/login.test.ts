/** Sign-in targets, their typed names, refused answers asked again, and signing out. */
import { describe, expect, it } from 'bun:test'
import type { Context } from '@deepseek-ai/cordis'
import { credentialKey } from 'bake-credentials'
import { availableSelection, findTarget, listTargets, login, logout, startingModel, type LoginPrompt, type LoginSources } from '../src/login.ts'
import { dictionaries } from 'bake-tui-ui/copy.ts'

const copy = dictionaries.en
const DEEPSEEK: LoginSources = { refs: [{ ref: 'DEEPSEEK_API_KEY', label: 'DeepSeek', provider: 'deepseek-official' }] }
const OPENAI = credentialKey('llm-pi-ai', 'openai')

interface Fixture {
  /** Stored key values, by reference. */
  readonly keys: Map<string, string>
  /** References the inherited environment supplies. */
  readonly env: Set<string>
  /** Stored flow records, by key. */
  readonly records: Set<string>
  /** The `llm-pi-ai` settings section. */
  section: { providers: Record<string, unknown> }
  readonly ops: unknown[]
  /** What each flow attempt was begun with. */
  readonly begun: { key: string, method?: string }[]
}

function fixture(options: { flows?: boolean, providers?: readonly string[], models?: Record<string, readonly string[]> } = {}): { ctx: Context, state: Fixture } {
  const state: Fixture = { keys: new Map(), env: new Set(), records: new Set(), section: { providers: {} }, ops: [], begun: [] }
  const ctx = {
    get(name: string) {
      if (name === 'credentials') return {
        describe: async (ref: string) => state.env.has(ref) ? { configured: true, source: 'env', writable: false }
          : state.keys.has(ref) ? { configured: true, source: 'file', writable: true } : { configured: false, writable: true },
        set: async (ref: string, value: string) => { state.keys.set(ref, value) },
        unset: async (ref: string) => { state.keys.delete(ref) },
        describeRecord: async (key: string) => ({ configured: state.records.has(key), writable: true }),
        deleteRecord: async (key: string) => { state.records.delete(key) },
      }
      if (name === 'settings') return {
        writable: true,
        get: (ns: string) => ns === 'llm-pi-ai' ? state.section : undefined,
        mutate: async (ns: string, ops: { op: string, path: string[], value?: unknown }[]) => {
          state.ops.push([ns, ops])
          for (const op of ops) {
            const id = op.path[1]!
            if (op.op === 'set') state.section.providers[id] = op.value
            else delete state.section.providers[id]
          }
        },
      }
      if (name === 'llm') return {
        listProviders: () => (options.providers ?? []).map(id => ({ id, name: id })),
        listModels: async (provider: string) => (options.models?.[provider] ?? []).map(id => ({ provider, id, name: id })),
      }
      if (name === 'authorization') return {
        list: () => options.flows === true ? [{
          key: OPENAI, label: 'OpenAI', inFlight: false,
          methods: [{ id: 'oauth', label: 'Sign in with ChatGPT' }, { id: 'api-key', label: 'OpenAI API key' }],
        }] : [],
        begin: async (request: { key: string, method?: string }) => {
          state.begun.push({ key: request.key, ...request.method === undefined ? {} : { method: request.method } })
          state.records.add(request.key)
          return { status: 'authorized' }
        },
      }
      return undefined
    },
  } as unknown as Context
  return { ctx, state }
}

/** Answer prompts in order, recording what each asked. */
function answering(answers: readonly (string | Error)[]) {
  const asked: LoginPrompt[] = []
  const queue = [...answers]
  return {
    asked,
    interaction: {
      notify: () => {},
      prompt: async (prompt: LoginPrompt) => {
        asked.push(prompt)
        const next = queue.shift()
        if (next === undefined || next instanceof Error) throw next ?? new Error('no answer queued')
        return next
      },
    },
  }
}

describe('sign-in targets', () => {
  it('names each target for people and for typing, with its standing', async () => {
    const { ctx, state } = fixture({ flows: true })
    state.env.add('DEEPSEEK_API_KEY')
    const targets = await listTargets(ctx, DEEPSEEK)
    expect(targets.map(target => [target.name, target.label, target.kind])).toEqual([
      ['deepseek', 'DeepSeek', 'key'], ['cliproxyapi', 'CLIProxyAPI', 'cliproxyapi'], ['openai', 'OpenAI', 'flow'],
    ])
    expect(targets[0]).toMatchObject({ id: 'DEEPSEEK_API_KEY', detail: 'DEEPSEEK_API_KEY', configured: true, source: 'env',
      writable: false, provider: 'deepseek-official' })
    expect(targets[2]).toMatchObject({ id: 'llm-pi-ai/openai', provider: 'openai', configured: false,
      detail: 'Sign in with ChatGPT \u00b7 OpenAI API key' })
  })

  it('offers only the flows the profile names, in its order', async () => {
    const { ctx } = fixture({ flows: true })
    expect((await listTargets(ctx, { refs: [], flows: [] })).map(target => target.name)).toEqual(['cliproxyapi'])
    expect((await listTargets(ctx, { refs: [], flows: ['llm-pi-ai/missing', 'llm-pi-ai/openai'] })).map(target => target.name))
      .toEqual(['cliproxyapi', 'openai'])
  })

  it('finds a target by name, address, or label in any case, and suggests the nearest', async () => {
    const { ctx } = fixture({ flows: true })
    const targets = await listTargets(ctx, DEEPSEEK)
    for (const typed of ['deepseek', 'DEEPSEEK_API_KEY', 'DeepSeek', ' CLIProxyAPI ', 'llm-pi-ai/openai']) {
      expect(findTarget(targets, typed).kind).toBe('found')
    }
    expect(findTarget(targets, 'deepsek')).toEqual({ kind: 'unknown', suggestion: 'deepseek' })
    expect(findTarget(targets, 'nothing-like-it')).toEqual({ kind: 'unknown' })
  })
})

describe('signing in', () => {
  it('asks again for a blank or unusable key, then stores the trimmed key', async () => {
    const { ctx, state } = fixture()
    const targets = await listTargets(ctx, DEEPSEEK)
    const { asked, interaction } = answering(['   ', 'two words', '  sk-good  '])
    const result = await login(ctx, targets, 'deepseek', interaction, new AbortController().signal, copy)
    expect(result).toMatchObject({ kind: 'stored', target: { id: 'DEEPSEEK_API_KEY' } })
    expect(state.keys.get('DEEPSEEK_API_KEY')).toBe('sk-good')
    expect(asked.map(prompt => [prompt.kind, prompt.title, prompt.error])).toEqual([
      ['secret', 'Sign in \u00b7 DeepSeek', undefined],
      ['secret', 'Sign in \u00b7 DeepSeek', copy.loginEmpty],
      ['secret', 'Sign in \u00b7 DeepSeek', copy.keyInvalid],
    ])
  })

  it('reports a key the environment supplies instead of prompting for it', async () => {
    const { ctx, state } = fixture()
    state.env.add('DEEPSEEK_API_KEY')
    const { asked, interaction } = answering([])
    expect(await login(ctx, await listTargets(ctx, DEEPSEEK), 'deepseek', interaction, new AbortController().signal, copy))
      .toMatchObject({ kind: 'read-only' })
    expect(asked).toEqual([])
  })

  it('keeps a declined sign-in local and names an unknown target with its nearest', async () => {
    const { ctx } = fixture()
    const targets = await listTargets(ctx, { refs: [] })
    const { interaction } = answering([new Error('user dismissed')])
    expect(await login(ctx, targets, 'cliproxyapi', interaction, new AbortController().signal, copy))
      .toMatchObject({ kind: 'cancelled', target: { id: 'cliproxyapi' } })
    expect(await login(ctx, targets, 'cliproxy', interaction, new AbortController().signal, copy))
      .toEqual({ kind: 'unknown-target', id: 'cliproxy', suggestion: 'cliproxyapi' })
  })

  it('lets a flow with several methods be signed into by the chosen one, and gives its provider a route', async () => {
    const { ctx, state } = fixture({ flows: true })
    const { asked, interaction } = answering(['api-key'])
    const result = await login(ctx, await listTargets(ctx, { refs: [] }), 'openai', interaction, new AbortController().signal, copy)
    expect(result).toMatchObject({ kind: 'stored', route: 'added', target: { provider: 'openai' } })
    expect(asked[0]).toMatchObject({ kind: 'select', title: 'Sign in \u00b7 OpenAI', message: copy.chooseMethod })
    expect(state.begun).toEqual([{ key: 'llm-pi-ai/openai', method: 'api-key' }])
    expect(state.section.providers).toEqual({ openai: {} })
    // A route that already stands is left as it is.
    state.section.providers['openai'] = { displayName: 'Mine' }
    const again = await login(ctx, await listTargets(ctx, { refs: [] }), 'openai', answering(['oauth']).interaction,
      new AbortController().signal, copy)
    expect(again).toMatchObject({ kind: 'stored', route: 'present' })
    expect(state.section.providers['openai']).toEqual({ displayName: 'Mine' })
  })
})

describe('signing out', () => {
  it('removes a stored key, and reports one the environment still supplies', async () => {
    const { ctx, state } = fixture()
    state.keys.set('DEEPSEEK_API_KEY', 'sk-good')
    expect(await logout(ctx, await listTargets(ctx, DEEPSEEK), 'deepseek')).toMatchObject({ kind: 'removed' })
    expect(state.keys.has('DEEPSEEK_API_KEY')).toBe(false)
    state.env.add('DEEPSEEK_API_KEY')
    expect(await logout(ctx, await listTargets(ctx, DEEPSEEK), 'deepseek')).toMatchObject({ kind: 'read-only' })
    expect(await logout(ctx, await listTargets(ctx, { refs: [] }), 'cliproxyapi')).toMatchObject({ kind: 'not-configured' })
  })

  it('removes the CLIProxyAPI route before its key', async () => {
    const { ctx, state } = fixture({ providers: ['cliproxyapi'] })
    state.keys.set('CLIPROXYAPI_API_KEY', 'proxy-key')
    state.section.providers['cliproxyapi'] = { baseURL: 'https://proxy.example/v1' }
    const targets = await listTargets(ctx, { refs: [] })
    expect(targets.find(target => target.name === 'cliproxyapi')).toMatchObject({ configured: true, detail: 'proxy.example' })
    expect(await logout(ctx, targets, 'CLIProxyAPI')).toMatchObject({ kind: 'removed' })
    expect(state.section.providers).toEqual({})
    expect(state.keys.has('CLIPROXYAPI_API_KEY')).toBe(false)
  })

  it('deletes a flow record, and the empty route its sign-in added but not a shaped one', async () => {
    const { ctx, state } = fixture({ flows: true })
    state.records.add('llm-pi-ai/openai')
    state.section.providers['openai'] = {}
    expect(await logout(ctx, await listTargets(ctx, { refs: [] }), 'openai')).toMatchObject({ kind: 'removed' })
    expect(state.records.size).toBe(0)
    expect(state.section.providers).toEqual({})
    state.records.add('llm-pi-ai/openai')
    state.section.providers['openai'] = { displayName: 'Mine' }
    await logout(ctx, await listTargets(ctx, { refs: [] }), 'openai')
    expect(state.section.providers).toEqual({ openai: { displayName: 'Mine' } })
  })
})

describe('the model a fresh session starts on', () => {
  const deepseek = { provider: 'deepseek-official', model: 'deepseek-flash' }

  it('starts on no model while nothing is signed in', async () => {
    const { ctx } = fixture({ models: { 'deepseek-official': ['deepseek-flash'] } })
    expect(await availableSelection(ctx, DEEPSEEK, undefined)).toBeUndefined()
    // A saved selection whose provider cannot answer is no better than none.
    expect(await availableSelection(ctx, DEEPSEEK, deepseek)).toBeUndefined()
  })

  it('keeps a saved selection whose provider is signed in, or that no target vouches for', async () => {
    const { ctx, state } = fixture({ models: { 'deepseek-official': ['deepseek-flash'] } })
    const gateway = { provider: 'my-gateway', model: 'm' }
    expect(await availableSelection(ctx, DEEPSEEK, gateway)).toEqual(gateway)
    state.keys.set('DEEPSEEK_API_KEY', 'sk-good')
    expect(await availableSelection(ctx, DEEPSEEK, deepseek)).toEqual(deepseek)
  })

  it('starts a route on the model the profile names when the route lists it, otherwise its first', async () => {
    const listed = [{ id: 'gpt-4' }, { id: 'gpt-5.5' }]
    expect(startingModel({ model: 'gpt-5.5' }, listed)).toBe('gpt-5.5')
    expect(startingModel({ model: 'retired' }, listed)).toBe('gpt-4')
    expect(startingModel({}, [])).toBeUndefined()
    const { ctx, state } = fixture({ flows: true, models: { openai: ['gpt-4', 'gpt-5.5'] } })
    state.records.add('llm-pi-ai/openai')
    expect(await availableSelection(ctx, { refs: [], flows: [{ key: 'llm-pi-ai/openai', model: 'gpt-5.5' }] }, undefined))
      .toEqual({ provider: 'openai', model: 'gpt-5.5' })
  })

  it('starts on the first signed-in provider that lists a model, in /login order', async () => {
    const { ctx, state } = fixture({ flows: true, providers: ['cliproxyapi'], models: { cliproxyapi: [], openai: ['gpt-6'] } })
    state.keys.set('CLIPROXYAPI_API_KEY', 'proxy-key')
    state.records.add('llm-pi-ai/openai')
    expect(await availableSelection(ctx, DEEPSEEK, undefined)).toEqual({ provider: 'openai', model: 'gpt-6' })
    expect(await availableSelection(ctx, DEEPSEEK, deepseek)).toEqual({ provider: 'openai', model: 'gpt-6' })
  })
})
