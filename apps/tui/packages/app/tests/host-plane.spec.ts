/**
 * The terminal profile's host plane as it ships: `bake-base` with the terminal
 * bundle's patch applied by the Loader's own patch semantics, every row the
 * composition leaves active mounted through the Loader, and the shipped
 * presets the runner offers. Only the model, the persistence root, the
 * workspace, and the terminal surface itself are replaced. A host row whose
 * tools or listeners reached preset agents would change what their requests
 * carry, so these cases read the requests: the skill catalog and a `/skill`
 * body once each, only `run_code` for `ptc`, only the shell for `minimal`,
 * and a `cordis` session that starts at all.
 */
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { join } from 'node:path'
import { afterEach, expect, it, vi } from 'vitest'
import { applyEntryPatches } from '@deepseek-ai/cordis-plugin-include'
import { ToolCallId, type GenerateOptions, type Message, type StreamChunk } from 'bake-llm'
import { defineContentToolFixture } from 'bake-tools'
import { transcriptRows } from 'bake-tui-ui'
import { composedProfile, shippedLayers } from './composed-profile.ts'
import { textResponse } from './harness.ts'

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
 * Boot the composed profile with a workspace holding marked instructions and one marked skill.
 * @returns the settled root context, the scripted model, and a session opener.
 */
async function profile() {
  return composedProfile(cleanup, {
    plant: async (workspace) => {
      await mkdir(join(workspace, '.agents', 'skills', SKILL), { recursive: true })
      // Skill discovery must stop here even when an ancestor of the temp root is a repository.
      await mkdir(join(workspace, '.git'))
      await writeFile(join(workspace, 'AGENTS.md'), `# Rules\n\n${INSTRUCTIONS_MARKER}: keep answers short.\n`)
      await writeFile(join(workspace, '.agents', 'skills', SKILL, 'SKILL.md'),
        `---\nname: ${SKILL}\ndescription: ${SKILL_MARKER} a workspace skill.\n---\n\nPLANE_SKILL_BODY: answer in one word.\n`)
    },
  })
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
  const layers = await shippedLayers()
  const rows = applyEntryPatches([], layers.flat(), () => {})
  const active = (id: string) => {
    const row = rows.find(entry => entry.id === id)
    if (row === undefined) throw new Error(`the composition has no ${id} row`)
    return row.disabled !== true
  }
  for (const id of [
    'agent-instructions', 'tool-bash', 'tool-pwsh', 'tool-fs', 'tool-fs-search', 'skill-filesystem', 'tool-skill',
    'command-goal', 'tool-goal', 'tool-subagent-control', 'tool-subagent-list-agents', 'tool-subagent',
    'tool-web', 'tool-jobs',
  ]) expect(active(id), id).toBe(false)
  // The registries and drivers behind those tools, `/compact` for `minimal`,
  // and the inspection registry the `cordis` preset's tools register into.
  for (const id of [
    'skill', 'goal', 'goal-round-driver', 'subagent', 'subagent-spawn-in-process',
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
  expect(first.tools?.map(tool => tool.name)).toEqual(expect.arrayContaining([SHELL, 'read', 'skill', 'job_output']))

  const [invoked] = await session.turn(`/${SKILL} go`)
  if (invoked === undefined) throw new Error('the invocation made no request')
  expect(carrying(invoked, `<skill_content name="${SKILL}">`)).toHaveLength(1)
  expect(carrying(invoked, 'PLANE_SKILL_BODY')).toHaveLength(1)
  expect(carrying(invoked, '<available_skills>')).toHaveLength(1)
  const log = await session.events()
  expect(log.filter(event => event.type === 'user/message' && event.data.source.kind === 'skill-invocation')).toHaveLength(1)
})

it('gives the ptc agent run_code alone, with its tools in the program interface', async () => {
  const { model, open } = await profile()
  model.response = async function* () { yield* textResponse('Recorded answer') }
  const session = await open('ptc')
  const [request] = await session.turn('Hello')
  if (request === undefined) throw new Error('the turn made no request')
  expect(request.tools?.map(tool => tool.name)).toEqual(['run_code'])
  // The program interface lists the agent's tools.
  expect(system(request)).toContain('job_output')
  expect(carrying(request, '<available_skills>')).toHaveLength(1)
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
    properties: { limit: { description: 'Maximum lines; default 2000.' } },
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

it('lists a background shell under the input while it runs, and closes it with a job row', async () => {
  const { model, open } = await profile()
  const call = { type: 'tool-call' as const, id: ToolCallId('call-background-1'), name: 'bash', arguments: JSON.stringify({ command: 'sleep 1', run_in_background: true }) }
  model.response = async function* (options: GenerateOptions): AsyncGenerator<StreamChunk> {
    // Start the job once, then answer its result and its completion notice.
    if (JSON.stringify(options.messages).includes(call.id)) {
      yield* textResponse('Recorded answer')
      return
    }
    yield { type: 'block-start', index: 0, blockType: 'tool-call' }
    yield { type: 'tool-call-delta', index: 0, id: call.id, name: call.name, argumentsDelta: call.arguments }
    yield { type: 'block-end', index: 0, block: call }
    yield { type: 'finish', reason: { kind: 'tool-calls' } }
  }
  const session = await open('standard')
  await session.turn('Start a background sleep')
  expect(session.controller.view.background).toEqual([{ id: expect.stringMatching(/^bash-\d+$/u), tool: 'bash', label: 'sleep 1', running: true }])
  await vi.waitFor(() => {
    expect(transcriptRows(session.controller.view.committed)).toContainEqual(
      expect.objectContaining({ kind: 'job-done', tool: 'bash', label: 'sleep 1', outcome: 'done' }))
  }, { timeout: 10000 })
  expect(session.controller.view.background.filter(entry => entry.running)).toEqual([])
})
