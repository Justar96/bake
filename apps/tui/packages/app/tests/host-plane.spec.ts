/**
 * The terminal profile's host plane as it ships: `dsh-base` with the terminal
 * bundle's patch applied by the Loader's own patch semantics, every row the
 * composition leaves active mounted through the Loader, and the shipped
 * presets the runner offers. Only the model, the persistence root, the
 * workspace, and the terminal surface itself are replaced. A host row whose
 * tools or listeners reached preset agents would change what their requests
 * carry, so these cases read the requests: the skill catalog and a `/skill`
 * body once each, no `workflow` for `ptc`, only the shell for `minimal`, and
 * a `cordis` session that starts at all.
 */
import { mkdir, mkdtemp, readFile, rm, symlink, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'
import * as yaml from 'js-yaml'
import { afterEach, expect, it, vi } from 'vitest'
import { Context } from '@deepseek-ai/cordis'
import Include, { applyEntryPatches, entryListSchema, type PatchOptions } from '@deepseek-ai/cordis-plugin-include'
import Loader, { Group } from '@deepseek-ai/cordis-plugin-loader'
import { dshHomePath } from '@deepseek-ai/dsh-home-paths'
import { ToolCallId, type GenerateOptions, type Message, type StreamChunk } from '@deepseek-ai/dsh-llm'
import type { SessionEvent } from '@deepseek-ai/dsh-session'
import { defineContentToolFixture } from '@deepseek-ai/dsh-tools'
import { dictionaries } from '@dsh-tui/ui/copy.ts'
import { SessionController } from '../src/controller.ts'
import { openSession } from '../src/session.ts'
import { ScriptedModel, textResponse } from './harness.ts'

const REPOSITORY = fileURLToPath(new URL('../../../../../', import.meta.url))

/** The layers the shipped `tui` profile applies over its empty root, in order. */
const LAYERS = ['packages/bundle/base/cordis.patch.yml', 'apps/tui/packages/app/cordis.built.patch.yml']

/** Markers the workspace plants where only a host row would put them in front of `minimal`. */
const INSTRUCTIONS_MARKER = 'PLANE_INSTRUCTIONS_MARKER'
const SKILL_MARKER = 'PLANE_SKILL_MARKER'
const SKILL = 'plane-skill'
const SHELL = process.platform === 'win32' ? 'pwsh' : 'bash'

/** About 1800 tokens, so one turn leaves history `/compact` can usefully fold. */
const CHUNK = 'Older conversation history that the summary can fold. '.repeat(135)

const cleanup: (() => Promise<void>)[] = []
afterEach(async () => {
  for (const dispose of cleanup.splice(0).reverse()) await dispose()
  vi.unstubAllEnvs()
})

/**
 * Boot the composed profile in a private home and workspace.
 * @returns the settled root context, the scripted model, and a session opener.
 */
async function profile() {
  const root = await mkdtemp(join(tmpdir(), 'dsh-tui-plane-'))
  const ctx = new Context()
  cleanup.push(async () => { await ctx.fiber.dispose(); await rm(root, { recursive: true, force: true }) })
  const workspace = join(root, 'workspace')
  await mkdir(join(workspace, '.agents', 'skills', SKILL), { recursive: true })
  // Skill discovery must stop here even when an ancestor of the temp root is a repository.
  await mkdir(join(workspace, '.git'))
  await writeFile(join(workspace, 'AGENTS.md'), `# Rules\n\n${INSTRUCTIONS_MARKER}: keep answers short.\n`)
  await writeFile(join(workspace, '.agents', 'skills', SKILL, 'SKILL.md'),
    `---\nname: ${SKILL}\ndescription: ${SKILL_MARKER} a workspace skill.\n---\n\nPLANE_SKILL_BODY: answer in one word.\n`)
  vi.stubEnv('DSH_HOME', join(root, 'home'))
  vi.stubEnv('DSH_AGENTS_HOME', join(root, 'agents'))

  const layers = await Promise.all(LAYERS.map(async path =>
    yaml.load(await readFile(join(REPOSITORY, path), 'utf8'), { schema: entryListSchema }) as PatchOptions[]))
  // What this test replaces: the terminal surface, the network model routes
  // and exporter, the title model call, the profile reload watcher, storage,
  // and the workspace.
  const overlay: PatchOptions[] = [
    ...['tui-startup', 'tui-runner', 'llm-deepseek', 'llm-pi-ai', 'session-title-llm', 'session-telemetry-otel', 'hmr']
      .map(id => ({ id, disabled: true })),
    { id: 'agent-default-model', config: { provider: 'mock', model: 'model' } },
    { id: 'session-persistence-jsonl', config: { root: join(root, 'sessions'), compression: 'none' } },
    { id: 'fs-sandbox', config: { cwd: workspace } },
    { id: 'sandbox-policy', config: { mode: 'workspace-write', workspaceRoot: workspace } },
  ]
  const skipped: string[] = []
  const rows = applyEntryPatches([], [...layers.flat(), ...overlay], message => { skipped.push(message) })
  expect(skipped).toEqual([])

  // Bare row names resolve from the Loader's base, here the private root, as
  // the launcher's resolve from the installation. Removing the root unlinks it.
  await mkdir(join(root, 'node_modules'))
  await symlink(join(REPOSITORY, 'node_modules', '@deepseek-ai'), join(root, 'node_modules', '@deepseek-ai'), 'junction')
  ctx.baseUrl = pathToFileURL(root).href + '/'
  ctx.provide('dshHomePath', dshHomePath)
  // What the launcher provides a profile it starts, here a private one. The
  // plugin manager mounts only under it, and the `cordis` preset's tool needs it.
  const profileDir = join(root, 'home', 'profiles', 'tui')
  ctx.provide('profileContext', {
    name: 'tui', dir: profileDir, patchPath: join(profileDir, 'cordis.patch.yml'),
    installAnchor: join(REPOSITORY, 'apps/cli/package.json'), cwd: workspace, home: join(root, 'home'),
    startedBundles: ['@deepseek-ai/dsh-base', '@dsh-tui/app'], overlays: [], telemetryDisabledEnv: undefined,
  })
  await ctx.plugin(Loader)
  ctx.loader.builtins.include = Include
  ctx.loader.builtins.group = Group
  for (const row of rows) await ctx.loader.create(row)
  await ctx.loader.await()

  const model = new ScriptedModel()
  ctx.llm.registerAdapter(['mock'], model)

  const open = async (preset: 'standard' | 'ptc' | 'minimal' | 'cordis') => {
    let controller!: SessionController
    const handle = await openSession(ctx, { preset }, new AbortController().signal, agent => {
      controller = new SessionController(ctx, agent, dictionaries.en, { refs: [] }, () => {},
        { attachmentMaxBytes: 1048576, attachmentLimit: 8 })
    })
    cleanup.push(async () => { controller.close(); await controller.drain(); await handle.dispose() })
    await controller.replay(new AbortController().signal)
    const turn = async (text: string) => {
      const before = model.requests.length
      expect(controller.submit(text)).toBe(true)
      await vi.waitFor(() => { expect(model.requests.length).toBeGreaterThan(before) })
      await handle.agent.whenIdle()
      return model.requests.slice(before)
    }
    const events = async (): Promise<readonly SessionEvent[]> => {
      using observation = await ctx.sessionQuery.observeSession(handle.agent.id, { projectionMode: 'none' })
      return [...observation.events]
    }
    return { controller, agent: handle.agent, turn, events }
  }
  return { ctx, model, open, workspace }
}

/** @returns one message's text. */
function text(message: Message): string {
  return typeof message.content === 'string'
    ? message.content
    : message.content.flatMap(block => block.type === 'text' ? [block.text] : []).join('\n')
}

/** @returns the request's non-system messages whose text contains `needle`. */
function carrying(request: GenerateOptions, needle: string): Message[] {
  return request.messages.filter(message => message.role !== 'system' && text(message).includes(needle))
}

/** @returns the request's system prompt. */
function system(request: GenerateOptions): string {
  return request.messages.filter(message => message.role === 'system').map(text).join('\n')
}

it('keeps only the compaction rows and the preset services on the terminal host plane', async () => {
  const layers = await Promise.all(LAYERS.map(async path =>
    yaml.load(await readFile(join(REPOSITORY, path), 'utf8'), { schema: entryListSchema }) as PatchOptions[]))
  const rows = applyEntryPatches([], layers.flat(), () => {})
  const active = (id: string) => {
    const row = rows.find(entry => entry.id === id)
    if (row === undefined) throw new Error(`the composition has no ${id} row`)
    return row.disabled !== true
  }
  for (const id of [
    'agent-instructions', 'tool-bash', 'tool-pwsh', 'tool-fs', 'tool-fs-search', 'skill-filesystem', 'tool-skill',
    'command-goal', 'tool-goal', 'tool-subagent-control', 'tool-subagent-list-agents', 'tool-subagent',
    'tool-subagent-fork', 'workflow-ptc', 'tool-workflow', 'tool-todo', 'tool-web', 'tool-jobs',
  ]) expect(active(id), id).toBe(false)
  // The registries and drivers behind those tools, `/compact` for `minimal`,
  // and the inspection registry the `cordis` preset's tools register into.
  for (const id of [
    'skill', 'goal', 'goal-round-driver', 'subagent', 'subagent-spawn-in-process', 'subagent-fork-in-process',
    'compaction-basic', 'command-compact', 'tool-result-pruner', 'cordis-host-runner',
  ]) expect(active(id), id).toBe(true)
})

it('gives a standard agent the skill catalog once, and a /skill invocation its body once', async () => {
  const { ctx, model, open } = await profile()
  // The terminal reads these host services directly.
  for (const name of ['goals', 'subagents', 'skills', 'compaction', 'sessionProjections']) expect(ctx.get(name), name).toBeDefined()
  model.response = async function* () { yield* textResponse('Recorded answer') }
  const session = await open('standard')
  // The slash menu finds the workspace skill through the preset's registry.
  await vi.waitFor(() => {
    expect(session.controller.view.completion.entries).toContainEqual(expect.objectContaining({ name: SKILL, kind: 'skill' }))
  })

  const [first] = await session.turn('Hello')
  if (first === undefined) throw new Error('the turn made no request')
  expect(carrying(first, '<available_skills>')).toHaveLength(1)
  expect(carrying(first, SKILL_MARKER)).toHaveLength(1)
  expect(carrying(first, INSTRUCTIONS_MARKER)).toHaveLength(1)
  expect(first.tools?.map(tool => tool.name)).toEqual(expect.arrayContaining([SHELL, 'read', 'skill', 'todo_write', 'workflow']))

  const [invoked] = await session.turn(`/${SKILL} go`)
  if (invoked === undefined) throw new Error('the invocation made no request')
  expect(carrying(invoked, `<skill_content name="${SKILL}">`)).toHaveLength(1)
  expect(carrying(invoked, 'PLANE_SKILL_BODY')).toHaveLength(1)
  expect(carrying(invoked, '<available_skills>')).toHaveLength(1)
  const log = await session.events()
  expect(log.filter(event => event.type === 'user/message' && event.data.source.kind === 'skill-invocation')).toHaveLength(1)
})

it('leaves workflow out of the ptc agent\'s run_code program interface', async () => {
  const { model, open } = await profile()
  model.response = async function* () { yield* textResponse('Recorded answer') }
  const session = await open('ptc')
  const [request] = await session.turn('Hello')
  if (request === undefined) throw new Error('the turn made no request')
  expect(request.tools?.map(tool => tool.name)).toEqual(['run_code'])
  // The program interface lists the agent's tools; the host rows' `workflow` would join them.
  expect(system(request)).toContain('todo_write')
  expect(system(request)).not.toMatch(/\bworkflow\b/u)
  expect(carrying(request, '<available_skills>')).toHaveLength(1)
})

it('shows the real workflow and its member while running, then its durable completion', async () => {
  const { model, open } = await profile()
  model.resolveModel = async (provider, model) => ({ provider, id: model, name: model, inputModalities: ['text'], context: { contextWindow: 128_000 } })
  const session = await open('standard')
  const release = Promise.withResolvers<void>()
  const call = { type: 'tool-call' as const, id: ToolCallId('workflow-call'), name: 'workflow',
    arguments: JSON.stringify({ meta: { name: 'review', description: 'Review changes' },
      script: 'phase("Inspect"); return await agent("Review files", { label: "Reviewer" });' }),
  }
  model.response = async function* (options) {
    if (options.sessionId !== session.agent.id) {
      await release.promise
      yield* textResponse('Review complete')
    } else if (JSON.stringify(options.messages).includes(call.id)) {
      yield* textResponse('Workflow complete')
    } else {
      yield { type: 'block-start', index: 0, blockType: 'tool-call' }
      yield { type: 'tool-call-delta', index: 0, id: call.id, name: call.name, argumentsDelta: call.arguments }
      yield { type: 'block-end', index: 0, block: call }
      yield { type: 'finish', reason: { kind: 'tool-calls' } }
    }
  }
  try {
    expect(session.controller.submit('Run a workflow to review the files')).toBe(true)
    await vi.waitFor(() => expect(session.controller.view.workflows).toEqual([
      expect.objectContaining({ name: 'review', state: 'working', total: 1, completed: 0 }),
    ]))
    await vi.waitFor(() => expect(session.controller.view.subagents).toContainEqual(
      expect.objectContaining({ workflow: 'review', state: 'working', detail: 'One-shot · Inspect' })))
    release.resolve()
    await session.agent.whenIdle()
    expect(session.controller.view.workflows).toEqual([
      expect.objectContaining({ name: 'review', state: 'completed', total: 1, completed: 1 }),
    ])
    const requests = model.requests.filter(request => request.sessionId === session.agent.id && request.purpose === undefined)
    expect(requests).toHaveLength(2)
    expect(requests[1]!.tools).toEqual(requests[0]!.tools)
    expect(requests[1]!.messages.slice(0, requests[0]!.messages.length)).toEqual(requests[0]!.messages)
  } finally { release.resolve(); await session.agent.whenIdle() }
})

it('bounds new file reads without changing tool schemas and keeps later pages available', async () => {
  const { ctx, open, workspace } = await profile()
  const session = await open('standard')
  const path = join(workspace, 'preview.txt')
  await writeFile(path, Array.from({ length: 1000 }, (_, index) => `${index + 1}: ${'x'.repeat(80)}`).join('\n'))
  const schemas = ctx.tools.schemas(session.agent)
  const read = (offset: number) => ctx.tools.execute({ name: 'read', callId: ToolCallId(`read-${offset}`),
    arguments: { file_path: path, offset }, agent: session.agent, signal: new AbortController().signal })
  const first = await read(1)
  expect(first.isError).toBe(false)
  expect(JSON.stringify(first.content)).not.toContain(`1000: ${'x'.repeat(80)}`)
  expect(JSON.stringify(first.content).length).toBeLessThan(20_000)
  const last = await read(1000)
  expect(last.isError).toBe(false)
  expect(JSON.stringify(last.content)).toContain(`1000: ${'x'.repeat(80)}`)
  expect(ctx.tools.schemas(session.agent)).toEqual(schemas)
  expect(schemas.find(schema => schema.name === 'read')?.parameters).toMatchObject({
    properties: { limit: { description: 'Maximum number of lines to return. Defaults to 2000.' } },
  })

  const output = 'complete tool output\n'.repeat(1000)
  ctx.tools.register(defineContentToolFixture({
    name: 'preview_fixture', description: 'Return fixture text.', parameters: {},
    async execute() { return [{ type: 'text', text: output }] },
  }))
  const preview = await ctx.tools.execute({ name: 'preview_fixture', callId: ToolCallId('preview'),
    arguments: {}, agent: session.agent, signal: new AbortController().signal })
  expect(preview.isError).toBe(false)
  const rendered = preview.content.flatMap(block => block.type === 'text' ? [block.text] : []).join('')
  expect(Buffer.byteLength(rendered)).toBeLessThanOrEqual(16384)
  const locator = /Full formatted result stored at: (.+?)\. /u.exec(rendered)?.[1]
  if (locator === undefined) throw new Error('the preview must name the complete saved output')
  expect(await readFile(locator, 'utf8')).toBe(output)
})

it('gives a minimal agent only its shell, and /compact still folds its history', async () => {
  const { model, open } = await profile()
  model.response = async function* (options: GenerateOptions): AsyncGenerator<StreamChunk> {
    if (options.purpose === 'compaction') {
      yield* textResponse('The earlier turn asked for history to be folded.')
      return
    }
    yield { type: 'usage', usage: { inputTokens: 900, outputTokens: 20 } }
    yield* textResponse('Recorded answer')
  }
  const session = await open('minimal')
  const [request] = await session.turn(CHUNK)
  if (request === undefined) throw new Error('the turn made no request')
  expect(request.tools?.map(tool => tool.name)).toEqual([SHELL])
  expect(system(request)).toBe('You are a helpful software engineer assistant.')
  // Nothing but the human's own prompt: no instructions, catalog, or runtime snapshot.
  expect(request.messages.filter(message => message.role === 'user').map(text)).toEqual([CHUNK])

  // No preset unit registers a task list here, and the status reads without it.
  expect(session.controller.view.todos).toBeUndefined()

  expect(session.controller.submit('/compact')).toBe(true)
  await session.controller.drain()
  const log = await session.events()
  expect(log.find(event => event.type === 'command/done')?.data)
    .toMatchObject({ kind: 'success', text: expect.stringMatching(/^Compacted \d+ history items/u) })
  expect(log.filter(event => event.type === 'compaction/summary')).toHaveLength(1)
})

it('starts a cordis session whose inspection tools reach the host registry', async () => {
  const { ctx, model, open } = await profile()
  expect(ctx.get('cordisInspect')).toBeDefined()
  const call = { type: 'tool-call' as const, id: ToolCallId('call-inspect-1'), name: 'cordis_inspect_list', arguments: '{}' }
  model.response = async function* (options: GenerateOptions): AsyncGenerator<StreamChunk> {
    // Call the tool once, then answer its result.
    if (JSON.stringify(options.messages).includes(call.id)) {
      yield* textResponse('Recorded answer')
      return
    }
    yield { type: 'block-start', index: 0, blockType: 'tool-call' }
    yield { type: 'tool-call-delta', index: 0, id: call.id, name: call.name, argumentsDelta: call.arguments }
    yield { type: 'block-end', index: 0, block: call }
    yield { type: 'finish', reason: { kind: 'tool-calls' } }
  }
  const session = await open('cordis')
  const [request] = await session.turn('List the inspect providers')
  if (request === undefined) throw new Error('the turn made no request')
  expect(request.tools?.map(tool => tool.name)).toEqual(expect.arrayContaining(['cordis_inspect_list', 'cordis_inspect_query', 'plugin_manager']))
  const result = (await session.events()).find(event => event.type === 'tool/result')
  expect(result?.data.message.content).toEqual([expect.objectContaining({ isError: false })])
  expect(JSON.stringify(result?.data.message.content)).toContain('listTools')
})
