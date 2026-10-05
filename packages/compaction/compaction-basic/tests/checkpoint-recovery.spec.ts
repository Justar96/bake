import { describe, expect, it } from 'vitest'
import { Context } from '@deepseek-ai/cordis'
import BasicCompactionEngine from '@deepseek-ai/dsh-compaction-basic'
import type { BasicCompactionConfig } from '@deepseek-ai/dsh-compaction-basic'
import { compactCheckpointSource, CompactionId } from 'bake-compaction'
import {
  collectCheckpointContext,
  formatCheckpointContext,
  MAX_LISTED_FILES,
} from '@deepseek-ai/dsh-compaction-basic/src/checkpoint-context.ts'
import {
  boundedSummarizationInput,
  serializeTranscript,
} from '@deepseek-ai/dsh-compaction-basic/src/bounded-input.ts'
import { frameSummary } from '@deepseek-ai/dsh-compaction-basic/src/summarizer.ts'
import { MAX_SUMMARY_RANGE_HALVINGS } from '@deepseek-ai/dsh-compaction-basic/src/region.ts'
import { MAX_SUMMARY_RETRIES } from '@deepseek-ai/dsh-compaction-basic/src/summary-retry.ts'
import LlmRuntime, {
  CONTEXT_WINDOW_EXCEEDED_CODE,
  createMessage,
  createToolResultMessage,
  createUserMessage,
  LlmAdapter,
  resolveRetryPolicy,
  ToolCallId,
} from '@deepseek-ai/dsh-llm'
import type {
  GenerateOptions,
  LlmFailure,
  LlmResolvedModelInfo,
  Message,
  ResolvedRetryPolicy,
  StreamChunk,
} from '@deepseek-ai/dsh-llm'
import SessionStore, { Session, SessionId } from 'bake-session'
import SessionProjectionRegistry from '@deepseek-ai/dsh-session-projection'
import TokenMeter from '@deepseek-ai/dsh-token-meter'
import { agentEvents, type Agent, type RequestErrorAction } from 'bake-agent'

const MODEL = 'test-model'
const SIGNAL = new AbortController().signal

/** One scripted summary response: a failure code, or successful text. */
type Reply = { readonly fail: string; readonly retryAfterMs?: number } | { readonly text: string }

/** Adapter that answers each summary call from a script and records its request. */
class ScriptedAdapter extends LlmAdapter {
  readonly requests: GenerateOptions[] = []
  readonly replies: Reply[] = []
  constructor(private readonly policy?: ResolvedRetryPolicy) {
    super()
  }

  override providerRetryPolicy(): ResolvedRetryPolicy | undefined {
    return this.policy
  }

  override resolveModel(provider: string, model: string): Promise<LlmResolvedModelInfo> {
    return Promise.resolve({ provider, id: model, name: model, context: { contextWindow: 100_000 } })
  }

  override async * stream(options: GenerateOptions): AsyncIterable<StreamChunk> {
    this.requests.push(options)
    const reply = this.replies.shift() ?? { text: 'summary' }
    if ('fail' in reply) {
      const failure: LlmFailure = {
        message: `failed with ${reply.fail}`,
        code: reply.fail,
        ...reply.retryAfterMs === undefined ? {} : { providerRetryAfterMs: reply.retryAfterMs },
      }
      yield { type: 'finish', reason: { kind: 'error', failure } }
      return
    }
    yield { type: 'block-start', index: 0, blockType: 'text' }
    yield { type: 'text-delta', index: 0, text: reply.text }
    yield { type: 'finish', reason: { kind: 'stop' } }
  }
}

const FAST_POLICY = resolveRetryPolicy({
  mode: 'normal',
  maxRetries: 5,
  backoff: { initialDelayMs: 1, maxDelayMs: 5, jitterRatio: 0 },
}, 'test retryPolicy')

async function harness(
  config: BasicCompactionConfig = { auto: false },
  policy: ResolvedRetryPolicy | undefined = FAST_POLICY,
): Promise<{ ctx: Context; adapter: ScriptedAdapter; compact: BasicCompactionEngine; warnings: string[] }> {
  const ctx = new Context()
  await ctx.plugin(LlmRuntime)
  await ctx.plugin(SessionProjectionRegistry)
  void new TokenMeter(ctx)
  const adapter = new ScriptedAdapter(policy)
  ctx.llm.registerAdapter([MODEL], adapter)
  const warnings: string[] = []
  ctx.logger.warn = ((message: string) => void warnings.push(message)) as typeof ctx.logger.warn
  const compact = new BasicCompactionEngine(ctx, config)
  return { ctx, adapter, compact, warnings }
}

function agent(session: Session): Agent {
  return { session, options: { provider: MODEL, model: MODEL } } as Agent
}

/** One tool call issued by the assistant, with its result. */
interface Call {
  readonly name: string
  readonly args: unknown
  readonly result?: string
  readonly isError?: boolean
}

/**
 * Closed turns, each one user message, one assistant step issuing `calls`,
 * and their results, followed by an open turn.
 */
function toolSession(
  turns: readonly (readonly Call[])[],
  session: Session = Session.create(SessionId('checkpoint-recovery')),
  openTurn = true,
): Session {
  const text = 'context '.repeat(40)
  let id = 0
  for (const [index, calls] of turns.entries()) {
    const turn = index + 1
    session.append('turn/start', { turn })
    session.append('user/message', createUserMessage({
      content: [{ type: 'text', text: `${text} request ${turn}` }],
      source: { kind: 'user' },
    }), { surfaceOp: 'append' })
    session.append('step/start', { turn, step: 1 })
    if (turn === 1) {
      session.append('request/header', {
        header: {
          config: { provider: MODEL, model: MODEL },
          tools: [{ name: 'read', description: 'r', parameters: { type: 'object' } }],
        },
        reason: 'initial',
      })
    }
    const ids = calls.map(() => ToolCallId(`call-${++id}`))
    session.append('assistant/message', {
      stream: [],
      turn,
      step: 1,
      message: createMessage({
        role: 'assistant',
        content: [
          { type: 'text', text: `${text} answer ${turn}` },
          ...calls.map((call, at) => ({
            type: 'tool-call' as const,
            id: ids[at]!,
            name: call.name,
            arguments: typeof call.args === 'string' ? call.args : JSON.stringify(call.args),
          })),
        ],
        source: { kind: 'model', provider: MODEL, model: MODEL },
      }),
    }, { surfaceOp: 'append' })
    for (const [at, call] of calls.entries()) {
      const callId = ids[at]!
      const args = typeof call.args === 'string' ? call.args : JSON.stringify(call.args)
      session.append('tool/call', { turn, step: 1, callId, name: call.name, arguments: args })
      session.append('tool/result', {
        turn,
        step: 1,
        message: createToolResultMessage({
          callId,
          content: [{ type: 'text', text: call.result ?? 'ok' }],
          isError: call.isError ?? false,
        }),
      }, { surfaceOp: 'append' })
    }
    session.append('step/end', { turn, step: 1 })
    session.append('turn/end', { turn, reason: { kind: 'completed' } })
  }
  if (openTurn) session.append('turn/start', { turn: turns.length + 1 })
  return session
}

/** Text of the newest checkpoint on the surface. */
function checkpointText(session: Session): string {
  const checkpoint = session.deriveMessages()
    .find(message => message.source.kind === 'plugin' && message.source.plugin === 'compact')
  return (checkpoint?.content ?? []).map(block => block.type === 'text' ? block.text : '').join('')
}

/** Text of a request's first message. */
function firstText(options: GenerateOptions | undefined): string {
  const block = options?.messages[0]?.content[0]
  return block?.type === 'text' ? block.text : ''
}

function assistantCalls(calls: readonly Call[]): Message {
  return createMessage({
    role: 'assistant',
    content: calls.map((call, index) => ({
      type: 'tool-call' as const,
      id: ToolCallId(`c-${index}`),
      name: call.name,
      arguments: typeof call.args === 'string' ? call.args : JSON.stringify(call.args),
    })),
    source: { kind: 'model', provider: MODEL, model: MODEL },
  })
}

describe('checkpoint working-state sections', () => {
  it('lists read and modified paths, newest touch last, and skips failed or malformed calls', () => {
    const messages: Message[] = [
      assistantCalls([
        { name: 'read', args: { file_path: 'src/a.ts' } },
        { name: 'read', args: { file_path: 'src/b.ts' } },
        { name: 'edit', args: { file_path: 'src/b.ts', edits: [{ old_string: 'x', new_string: 'y' }] } },
        { name: 'write', args: { file_path: ' src/new.ts ', content: 'x' } },
        { name: 'read_image', args: { file_path: 'shot.png' } },
        { name: 'edit', args: { file_path: 'src/failed.ts', old_string: 'a', new_string: 'b' } },
        { name: 'read', args: '{not json' },
        { name: 'read', args: { file_path: 'multi\nline' } },
        { name: 'bash', args: { command: 'cat src/c.ts' } },
      ]),
      createToolResultMessage({ callId: ToolCallId('c-5'), content: [{ type: 'text', text: 'no match' }], isError: true }),
    ]

    const context = collectCheckpointContext(messages)

    expect(context).toEqual({
      readFiles: ['src/a.ts', 'shot.png'],
      modifiedFiles: ['src/b.ts', 'src/new.ts'],
    })
    expect(formatCheckpointContext(context)).toEqual([{
      type: 'text',
      text: '<read-files>\nsrc/a.ts\nshot.png\n</read-files>\n\n<modified-files>\nsrc/b.ts\nsrc/new.ts\n</modified-files>',
    }])
  })

  it('reads the file lists of a released checkpoint that also carries a todo list, and drops the list', () => {
    // Checkpoints written before the todo list was removed end with a
    // `<todo-list>` section; it is opaque text now and is not carried forward.
    const prior = createUserMessage({
      content: frameSummary([{ type: 'text', text: 'summary' }], [{
        type: 'text',
        text: '<read-files>\nold.ts\n</read-files>\n\n<todo-list>\n- [pending] Carry me\n</todo-list>',
      }]),
      source: compactCheckpointSource(CompactionId('released')),
    })

    const context = collectCheckpointContext([
      prior,
      assistantCalls([{ name: 'todo_write', args: { todos: [{ content: 'Ship', status: 'pending' }] } }]),
    ])

    expect(context).toEqual({ readFiles: ['old.ts'], modifiedFiles: [] })
    expect(formatCheckpointContext(context)).toEqual([{ type: 'text', text: '<read-files>\nold.ts\n</read-files>' }])
  })

  it('carries a prior checkpoint forward, with a later modification removing a read path', () => {
    const prior = createUserMessage({
      content: frameSummary(
        [{ type: 'text', text: 'summary mentioning <read-files>\nfake.ts\n</read-files>' }],
        formatCheckpointContext({
          readFiles: ['old-read.ts', 'later-edited.ts'],
          modifiedFiles: ['old-edit.ts'],
        }),
      ),
      source: compactCheckpointSource(CompactionId('prior')),
    })

    const context = collectCheckpointContext([
      prior,
      assistantCalls([{ name: 'edit', args: { file_path: 'later-edited.ts', old_string: 'a', new_string: 'b' } }]),
    ])

    expect(context).toEqual({
      readFiles: ['old-read.ts'],
      modifiedFiles: ['old-edit.ts', 'later-edited.ts'],
    })
  })

  it('caps each list at its newest paths and states how many older ones were omitted', () => {
    const calls = Array.from({ length: MAX_LISTED_FILES + 3 }, (_, index) => ({
      name: 'read', args: { file_path: `f${index}.ts` },
    }))
    const [block] = formatCheckpointContext(collectCheckpointContext([assistantCalls(calls)]))
    const lines = (block?.type === 'text' ? block.text : '').split('\n')

    expect(lines[0]).toBe('<read-files>')
    expect(lines[1]).toBe('... 3 earlier paths not shown')
    expect(lines[2]).toBe('f3.ts')
    expect(lines).toHaveLength(MAX_LISTED_FILES + 3)

    const prior = createUserMessage({
      content: frameSummary([{ type: 'text', text: 's' }], block === undefined ? [] : [block]),
      source: compactCheckpointSource(CompactionId('capped')),
    })
    expect(collectCheckpointContext([prior]).readFiles).toHaveLength(MAX_LISTED_FILES)
  })

  it('appends the sections after the summary block of a landed checkpoint', async () => {
    const { compact } = await harness()
    const session = toolSession([
      [
        { name: 'read', args: { file_path: 'README.md' } },
        { name: 'todo_write', args: { todos: [{ content: 'Fix bug', status: 'in_progress' }] } },
      ],
      [{ name: 'edit', args: { file_path: 'src/index.ts', old_string: 'a', new_string: 'b' } }],
      [],
    ])
    const range = session.surface.nodes
    // Compact the first two turns: each is user, assistant, and its results.
    const end = range[range.length - 3]
    await compact.compactRegion(range[0]!, end!, agent(session), SIGNAL)

    expect(checkpointText(session).endsWith([
      '</compacted-summary><read-files>',
      'README.md',
      '</read-files>',
      '',
      '<modified-files>',
      'src/index.ts',
      '</modified-files>',
    ].join('\n'))).toBe(true)
  })
})

describe('bounded summarizer input after a context-window overflow', () => {
  it('serializes a transcript with long tool output cut at 2,000 characters', () => {
    const messages: Message[] = [
      createUserMessage({ content: [{ type: 'text', text: 'please fix' }], source: { kind: 'user' } }),
      createMessage({
        role: 'assistant',
        content: [
          { type: 'reasoning', text: 'think' },
          { type: 'text', text: 'looking' },
          { type: 'tool-call', id: ToolCallId('t1'), name: 'read', arguments: '{"file_path":"a.ts"}' },
        ],
        source: { kind: 'model', provider: MODEL, model: MODEL },
      }),
      createToolResultMessage({ callId: ToolCallId('t1'), content: [{ type: 'text', text: 'Y'.repeat(2_005) }], isError: false }),
      createToolResultMessage({ callId: ToolCallId('t2'), content: [{ type: 'text', text: 'boom' }], isError: true }),
    ]

    expect(serializeTranscript(messages)).toBe([
      '[User]: please fix',
      '[Assistant reasoning]: think',
      '[Assistant]: looking',
      '[Assistant tool call]: read({"file_path":"a.ts"})',
      `[Tool result]: ${'Y'.repeat(2_000)}\n[... 5 more characters truncated]`,
      '[Tool error]: boom',
    ].join('\n\n'))
    const input = boundedSummarizationInput(messages)
    expect(input.tools).toBeUndefined()
    expect(input.messages).toHaveLength(1)
    expect(firstText({ messages: input.messages } as GenerateOptions)).toBe(
      'The conversation to condense is serialized below as a transcript. Tool results, tool-call arguments, and reasoning longer than 2,000 characters are cut, with a marker giving the number of characters removed.\n\n'
      + `<conversation>\n${serializeTranscript(messages)}\n</conversation>`,
    )
  })

  it('retries an overflowing replay as a bounded transcript of the same span', async () => {
    const { adapter, compact, warnings } = await harness()
    adapter.replies.push({ fail: CONTEXT_WINDOW_EXCEEDED_CODE })
    const session = toolSession([
      [{ name: 'read', args: { file_path: 'big.txt' }, result: 'Z'.repeat(5_000) }],
      [],
    ])
    const nodes = [...session.surface.nodes]

    const result = await compact.compactIfNeeded(agent(session), 'context-overflow', SIGNAL)

    expect(adapter.requests).toHaveLength(2)
    expect(adapter.requests[0]?.tools).toBeDefined()
    expect(adapter.requests[1]?.tools).toBeUndefined()
    expect(adapter.requests[1]?.messages).toHaveLength(2)
    const transcript = firstText(adapter.requests[1])
    expect(transcript).toContain('<conversation>\n[User]: ')
    expect(transcript).toContain('[... 3000 more characters truncated]')
    expect(result?.shadowedSeqs).toEqual(nodes.slice(0, -1))
    expect(checkpointText(session)).toContain('<read-files>\nbig.txt\n</read-files>')
    expect(warnings).toContainEqual(expect.stringContaining('retrying with a bounded transcript of 4 of 4 surface nodes'))
  })

  it('halves a still-overflowing transcript span toward its older end without splitting a tool pair', async () => {
    const { adapter, compact } = await harness()
    adapter.replies.push({ fail: CONTEXT_WINDOW_EXCEEDED_CODE }, { fail: CONTEXT_WINDOW_EXCEEDED_CODE })
    const session = toolSession([
      [{ name: 'read', args: { file_path: 'one.ts' } }],
      [{ name: 'read', args: { file_path: 'two.ts' } }],
      [],
    ])
    const nodes = [...session.surface.nodes]

    const result = await compact.compactIfNeeded(agent(session), 'context-overflow', SIGNAL)

    // Eight compactable nodes; a cut after turn 2's user message would split
    // its step, so the older half ends at turn 1's tool result.
    expect(adapter.requests).toHaveLength(3)
    expect(result?.shadowedSeqs).toEqual(nodes.slice(0, 3))
    expect(checkpointText(session)).toContain('<read-files>\none.ts\n</read-files>')
    expect(checkpointText(session)).not.toContain('two.ts')
  })

  it('gives up with the overflow after the bounded attempts and records the failed bracket', async () => {
    const { adapter, compact } = await harness()
    for (let index = 0; index < 20; index += 1) adapter.replies.push({ fail: CONTEXT_WINDOW_EXCEEDED_CODE })
    const turns = Array.from({ length: 16 }, () => [] as Call[])
    const session = toolSession(turns)
    const before = [...session.surface.nodes]

    await expect(compact.compactIfNeeded(agent(session), 'context-overflow', SIGNAL))
      .rejects.toMatchObject({ code: CONTEXT_WINDOW_EXCEEDED_CODE })

    expect(adapter.requests).toHaveLength(2 + MAX_SUMMARY_RANGE_HALVINGS)
    expect(session.surface.nodes).toEqual(before)
    expect(session.snapshotEvents().at(-1)).toMatchObject({ type: 'compaction/end', data: { error: expect.any(String) } })
  })

  it('lets canonical overflow recovery retry the request after a bounded summary lands', async () => {
    const { ctx, adapter } = await harness({ auto: true })
    adapter.replies.push({ fail: CONTEXT_WINDOW_EXCEEDED_CODE })
    const session = toolSession([[], []])
    const owner = agent(session)

    const decision = await agentEvents(ctx, owner).waterfall(
      'agent/request-error',
      {
        turn: 3,
        step: 1,
        provider: MODEL,
        failure: { message: 'request overflow', code: CONTEXT_WINDOW_EXCEEDED_CODE },
        retryPolicy: undefined,
        signal: SIGNAL,
      },
      () => Promise.resolve<RequestErrorAction>(undefined),
    )

    expect(decision).toEqual({ kind: 'retry' })
    expect(adapter.requests).toHaveLength(2)
    expect(session.snapshotEvents().some(event => event.type === 'compaction/summary')).toBe(true)
  })
})

describe('transient summary retries', () => {
  it('retries a retryable overflow-recovery summary failure under the provider policy', async () => {
    const { adapter, compact, warnings } = await harness()
    adapter.replies.push({ fail: 'RATE_LIMIT' }, { fail: 'SERVER', retryAfterMs: 2 })
    const session = toolSession([[], []])

    const result = await compact.compactIfNeeded(agent(session), 'context-overflow', SIGNAL)

    expect(result).not.toBeNull()
    expect(adapter.requests).toHaveLength(3)
    // Every attempt replays the same warm prefix.
    expect(adapter.requests[2]?.messages.slice(0, -1)).toEqual(adapter.requests[0]?.messages.slice(0, -1))
    expect(adapter.requests[2]?.tools).toEqual(adapter.requests[0]?.tools)
    expect(warnings).toEqual([
      'compaction summary failed: failed with RATE_LIMIT; retry 1 in 1 ms',
      'compaction summary failed: failed with SERVER; retry 2 in 2 ms',
    ])
  })

  it('stops at the retry bound and does not retry a non-retryable code', async () => {
    const bounded = await harness()
    for (let index = 0; index < 10; index += 1) bounded.adapter.replies.push({ fail: 'TIMEOUT' })
    await expect(bounded.compact.compactIfNeeded(agent(toolSession([[], []])), 'context-overflow', SIGNAL))
      .rejects.toMatchObject({ code: 'TIMEOUT' })
    expect(bounded.adapter.requests).toHaveLength(1 + MAX_SUMMARY_RETRIES)

    const strict = await harness({ auto: false }, resolveRetryPolicy({
      mode: 'normal', maxRetries: 1, backoff: { initialDelayMs: 1, maxDelayMs: 5, jitterRatio: 0 },
    }, 'strict'))
    strict.adapter.replies.push({ fail: 'TIMEOUT' }, { fail: 'TIMEOUT' })
    await expect(strict.compact.compactIfNeeded(agent(toolSession([[], []])), 'context-overflow', SIGNAL))
      .rejects.toMatchObject({ code: 'TIMEOUT' })
    expect(strict.adapter.requests).toHaveLength(2)

    const fatal = await harness()
    fatal.adapter.replies.push({ fail: 'AUTH' })
    await expect(fatal.compact.compactIfNeeded(agent(toolSession([[], []])), 'context-overflow', SIGNAL))
      .rejects.toMatchObject({ code: 'AUTH' })
    expect(fatal.adapter.requests).toHaveLength(1)
  })

  it('declines a provider-requested delay beyond the policy ceiling', async () => {
    const { adapter, compact } = await harness()
    adapter.replies.push({ fail: 'RATE_LIMIT', retryAfterMs: 60_000 })
    await expect(compact.compactIfNeeded(agent(toolSession([[], []])), 'context-overflow', SIGNAL))
      .rejects.toMatchObject({ code: 'RATE_LIMIT' })
    expect(adapter.requests).toHaveLength(1)
  })

  it('leaves pressure compaction to retry at its next step', async () => {
    const { adapter, compact } = await harness({ auto: false, thresholdTokens: 200, retainTokens: 50 })
    adapter.replies.push({ fail: 'RATE_LIMIT' })
    const session = toolSession([[], [], [], []])

    await expect(compact.compactIfNeeded(agent(session), 'pressure', SIGNAL))
      .rejects.toMatchObject({ code: 'RATE_LIMIT' })
    expect(adapter.requests).toHaveLength(1)
  })

  it('cancels a backoff wait without another summary call', async () => {
    const slow = resolveRetryPolicy({
      mode: 'normal', maxRetries: 3, backoff: { initialDelayMs: 60_000, maxDelayMs: 60_000, jitterRatio: 0 },
    }, 'slow')
    const { adapter, compact, warnings } = await harness({ auto: false }, slow)
    adapter.replies.push({ fail: 'RATE_LIMIT' })
    const controller = new AbortController()
    const session = toolSession([[], []])

    const pending = compact.compactIfNeeded(agent(session), 'context-overflow', controller.signal)
    await expect.poll(() => warnings.length).toBe(1)
    controller.abort(new Error('turn cancelled'))

    await expect(pending).rejects.toThrow('turn cancelled')
    expect(adapter.requests).toHaveLength(1)
    expect(session.snapshotEvents().at(-1)).toMatchObject({ type: 'compaction/end' })
  })

  it('ends a backoff wait when the plugin is disposed', async () => {
    const slow = resolveRetryPolicy({
      mode: 'normal', maxRetries: 3, backoff: { initialDelayMs: 60_000, maxDelayMs: 60_000, jitterRatio: 0 },
    }, 'slow')
    const ctx = new Context()
    await ctx.plugin(LlmRuntime)
    await ctx.plugin(SessionStore)
    await ctx.plugin(SessionProjectionRegistry)
    await ctx.plugin(TokenMeter)
    const adapter = new ScriptedAdapter(slow)
    ctx.llm.registerAdapter([MODEL], adapter)
    const warnings: string[] = []
    ctx.logger.warn = ((message: string) => void warnings.push(message)) as typeof ctx.logger.warn
    const fiber = await ctx.plugin(BasicCompactionEngine, { auto: false })
    const compact = ctx.get('compaction') as BasicCompactionEngine
    adapter.replies.push({ fail: 'RATE_LIMIT' })

    const pending = compact.compactIfNeeded(agent(toolSession([[], []])), 'context-overflow', SIGNAL)
    await expect.poll(() => warnings.length).toBe(1)
    await fiber.dispose()

    await expect(pending).rejects.toMatchObject({ code: 'RATE_LIMIT' })
    expect(adapter.requests).toHaveLength(1)
  })

  it('retries a transient failure of a manual compaction', async () => {
    const { ctx, adapter, compact, warnings } = await harness()
    await ctx.plugin(SessionStore)
    const session = toolSession([[], []], ctx.sessions.create(SessionId('manual-retry')), false)
    const owner = {
      ...agent(session),
      runMaintenance: <T>(work: (signal: AbortSignal) => Promise<T>) => work(new AbortController().signal),
    } as unknown as Agent
    adapter.replies.push({ fail: 'TRANSPORT' })

    const result = await compact.compactNow(owner, SIGNAL)

    expect(result).not.toBeNull()
    expect(adapter.requests).toHaveLength(2)
    expect(warnings).toEqual(['compaction summary failed: failed with TRANSPORT; retry 1 in 1 ms'])
  })
})
