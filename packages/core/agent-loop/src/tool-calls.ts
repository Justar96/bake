/**
 * Schedules one assistant step's tool calls. Exclusive calls form barriers;
 * parallel calls use a bounded rolling pool and are reclassified before start.
 * Dispatch may overlap, while policy, results, and result context remain
 * model-ordered. Abort or an internal scheduler failure stops replenishment
 * and drains started calls.
 *
 * Abort and a policy halt record synthetic error results for skipped calls so
 * replay stays valid. A terminal scheduler failure preserves already-recorded
 * `tool/call` events without fabricating results.
 * @module dsh-agent-loop/tool-calls
 */

import type { Context } from '@deepseek-ai/cordis'
import { createToolResultMessage, type ToolCallBlock } from '@deepseek-ai/dsh-llm'
import type { Session, SessionSeq, UserMessage } from '@deepseek-ai/dsh-session'
import { TOOL_ABORTED_BEFORE_DISPATCH, TOOL_RUNTIME_SCHEDULER, type ToolExecutionInput, type ToolExecutionMode, type ToolExecutionResult, type ToolHalt, type ToolRunContext, type ToolRuntimeScheduler } from '@deepseek-ai/dsh-tools'
import { assertNever } from '@deepseek-ai/dsh-util-values'
import type { ToolProgress } from '@deepseek-ai/dsh-agent'
import { ProgressThrottle } from './tool-progress.ts'

/** One tool call after argument parsing, ready to schedule. */
interface PlannedCall {
  block: ToolCallBlock
  exec: ToolExecutionInput
}

/** Settled dispatch awaiting model-order finalization. */
interface Slot {
  exec: ToolRunContext
  result: ToolExecutionResult
  needsPost: boolean
}

/** One scheduler group outcome, including a drained cancellation. */
interface GroupOutcome {
  consumed: number
  aborted: boolean
  /** Whether any committed result carried {@link ToolExecutionResult.concludesTurn}. */
  concluded: boolean
  /** The first halt a prepared or committed result carried; it stopped new starts. */
  halt?: ToolHalt
}

/**
 * Process-local publication of started calls' live state, outside the
 * model-ordered commit. Both callbacks are display-only: the durable
 * `tool/call` and `tool/result` order and every model request are unchanged.
 */
export interface ToolCallNotifier {
  /**
   * A running call's coalesced progress snapshot; never called after
   * {@link executed} for the same call.
   */
  progress(callId: ToolCallBlock['id'], progress: ToolProgress): void
  /** A started call finished executing, before its ordered commit; `isError` precedes post-execute. */
  executed(callId: ToolCallBlock['id'], isError: boolean): void
}

/** What one step's tool batch reports to the turn driver. */
export interface ToolBatchOutcome {
  /** Whether any committed result carried {@link ToolExecutionResult.concludesTurn}. */
  concluded: boolean
  /** The first halt a result carried; the batch started no call after detecting it. */
  halt?: ToolHalt
}

/**
 * Schedule one assistant step's tool calls by their live concurrency mode.
 * Ordinary completion and abort commit started-call results in order. Abort
 * drains them, records synthetic results for unstarted calls, and returns with
 * the signal still aborted after accepting started-call context through the
 * caller-supplied acceptor (the machine stages it in its next-step inbox for the
 * step boundary). A result carrying {@link ToolHalt} settles the same way
 * without aborting the signal: started calls run to completion, unstarted calls
 * receive synthetic results, and the halt is returned for the caller to end the
 * turn. An internal scheduler failure stops new dispatches, drains
 * already-started dispatches, and rejects with the first failure without
 * fabricating tool results.
 * The committed step's AgentLoop driver boundary supplies the initiating Agent
 * that becomes each explicit {@link ToolExecutionInput.agent}.
 *
 * @param ctx - loop context that owns the tool registry and carries the initiating Agent.
 * @param turn - current turn number.
 * @param step - current step number.
 * @param toolCalls - assistant calls in model order.
 * @param signal - abort signal shared by the step.
 * @param acceptContext - accepts committed result context for the next step boundary.
 * @param notify - receives each started call's live progress and its finish as it happens.
 */
export async function executeToolCalls(
  ctx: Context,
  turn: number,
  step: number,
  toolCalls: ToolCallBlock[],
  signal: AbortSignal,
  acceptContext: (context: UserMessage) => void,
  notify?: ToolCallNotifier,
): Promise<ToolBatchOutcome> {
  const agent = ctx.agents.requireInitiator()
  const { session } = agent
  // Cordis resolves services through the live context. Keep the scheduler for
  // this step so teardown after a logged call cannot replace it with undefined.
  const tools = ctx.tools
  const scheduler = tools[TOOL_RUNTIME_SCHEDULER]
  const executionMode = (exec: ToolExecutionInput): ToolExecutionMode => tools.executionMode(exec)

  // Inputs are distinct because tools/execute wrappers may replace `exec.signal`.
  const planned: PlannedCall[] = toolCalls.map(block => ({
    block,
    exec: {
      callId: block.id,
      name: block.name,
      arguments: parseArguments(block.arguments),
      agent,
      signal,
    },
  }))

  let next = 0
  let concluded = false
  while (next < planned.length) {
    // Commit before classifying again so registry changes affect unstarted calls.
    // oxlint-disable-next-line typescript/no-non-null-assertion -- bounded by the loop condition
    const first = planned[next]!
    const mode = executionMode(first.exec).kind
    const group = mode === 'parallel' ? planned.slice(next) : [first]
    const outcome = await runGroup(
      ctx, scheduler, executionMode, turn, step, group, mode, signal, acceptContext, notify,
    )
    next += outcome.consumed
    concluded ||= outcome.concluded
    if (outcome.aborted || outcome.halt !== undefined) {
      for (const call of planned.slice(next)) appendSkippedToolCall(session, turn, step, call.block)
      return { concluded, ...outcome.halt === undefined ? {} : { halt: outcome.halt } }
    }
  }
  return { concluded }
}

/** Parse model arguments, preserving invalid JSON as text and mapping empty input to `{}`. */
function parseArguments(raw: string): unknown {
  try {
    return raw ? JSON.parse(raw) : {}
  } catch {
    return raw
  }
}

/**
 * Run one exclusive barrier or parallel pool. Later calls are reclassified
 * before start; an exclusive reclassification waits for the current pool to
 * drain and remains for the caller's next barrier. Results and contexts commit
 * in model order. Abort stops starts, drains and commits started calls, accepts
 * their contexts into the owning batch, records results for skipped calls, and
 * returns an aborted outcome. A halt, seen on a prepared result or on a
 * committed one, does the same while started calls keep their live signal.
 * Scheduler failure drains dispatches without committing synthetic recovery
 * results.
 */
async function runGroup(
  ctx: Context,
  scheduler: ToolRuntimeScheduler,
  executionMode: (exec: ToolExecutionInput) => ToolExecutionMode,
  turn: number,
  step: number,
  group: PlannedCall[],
  mode: ToolExecutionMode['kind'],
  signal: AbortSignal,
  acceptContext: (context: UserMessage) => void,
  notify: ToolCallNotifier | undefined,
): Promise<GroupOutcome> {
  const { session } = ctx.agents.requireInitiator()
  const { maxParallelToolCalls } = ctx.agentLoop.config
  const slots: (Slot | undefined)[] = group.map(() => undefined)
  // Started slots retain their `tool/call` seq so the result can cite it.
  const callSeqs: Array<SessionSeq | undefined> = group.map(() => undefined)
  let nextToStart = 0
  let committed = 0
  let started = 0
  let aborted: boolean = signal.aborted
  let concluded = false
  let halt: ToolHalt | undefined
  let schedulerFailure: { error: unknown } | undefined
  const throwSchedulerFailure = (): void => {
    if (schedulerFailure !== undefined) throw schedulerFailure.error
  }

  // `committed` advances only across contiguous model-order slots.
  const commitReady = async (): Promise<void> => {
    while (committed < group.length) {
      const slot = slots[committed]
      if (slot === undefined) break
      const call = group[committed]
      const result = slot.needsPost
        ? await scheduler.finalize(slot.exec, slot.result)
        : scheduler.finish(slot.exec, slot.result)
      // oxlint-disable-next-line typescript/no-non-null-assertion -- bounded index
      appendToolResult(session, turn, step, call!.block, result, callSeqs[committed]!)
      for (const context of result.additionalContexts ?? []) acceptContext(context)
      concluded ||= result.concludesTurn === true
      halt ??= result.halt
      committed++
    }
  }

  const inFlight = new Map<number, Promise<number>>()
  // Each started call's progress publisher, closed as its dispatch settles
  // and again when the group returns or throws, so no timer outlives it.
  const throttles = new Set<ProgressThrottle>()

  const startCall = async (index: number): Promise<void> => {
    // oxlint-disable-next-line typescript/no-non-null-assertion -- bounded index
    const call = group[index]!
    callSeqs[index] = appendToolCall(session, turn, step, call.block)
    started++
    const callId = call.block.id
    const throttle = notify === undefined ? undefined
      : new ProgressThrottle((progress) => { notify.progress(callId, progress) })
    if (throttle !== undefined) throttles.add(throttle)
    const executed = (isError: boolean): void => {
      throttle?.close()
      notify?.executed(callId, isError)
    }
    const prepared = await scheduler.prepare(throttle === undefined ? call.exec
      : { ...call.exec, onProgress: (progress) => { throttle.push(progress) } })
    throwSchedulerFailure()
    // A pre-execute halt stops later starts before this result can commit
    // behind still-running earlier calls.
    if (prepared.kind !== 'dispatch') halt ??= prepared.result.halt
    switch (prepared.kind) {
      case 'dispatch': {
        const promise = scheduler.dispatch(prepared.exec).then(
          (outcome) => {
            slots[index] = { exec: prepared.exec, result: outcome.result, needsPost: outcome.kind === 'post-result' }
            executed(outcome.result.isError)
            return index
          },
          (error: unknown) => {
            throttle?.close()
            schedulerFailure ??= { error }
            return index
          },
        )
        inFlight.set(index, promise)
        break
      }
      case 'post-result':
        slots[index] = { exec: prepared.exec, result: prepared.result, needsPost: true }
        executed(prepared.result.isError)
        break
      case 'final-result':
        slots[index] = { exec: prepared.exec, result: prepared.result, needsPost: false }
        executed(prepared.result.isError)
        break
      /* v8 ignore next -- closed-union exhaustiveness guard */
      default:
        assertNever(prepared, 'tool-call scheduler prepare result')
    }
  }

  const fillPool = async (): Promise<void> => {
    while (!aborted && halt === undefined && nextToStart < group.length && inFlight.size < maxParallelToolCalls) {
      // Re-read later modes after ordered commits so registry changes can create a barrier.
      // oxlint-disable-next-line typescript/no-non-null-assertion -- bounded by the loop condition
      const nextCall = group[nextToStart]!
      if (nextToStart > 0 && mode === 'parallel'
        && executionMode(nextCall.exec).kind !== 'parallel') break
      await startCall(nextToStart)
      nextToStart++
      throwSchedulerFailure()
      await commitReady()
      throwSchedulerFailure()
      // Abort may arrive while pre-execute awaits.
      if (signal.aborted) aborted = true
    }
  }

  // Ordered pre-execute may await; only dispatch/body overlaps. A scheduler
  // failure stops new dispatches and reaches the turn boundary after every
  // already-started dispatch settles.
  try {
    await fillPool()
    while (inFlight.size > 0) {
      const settledIndex = await Promise.race(inFlight.values())
      inFlight.delete(settledIndex)
      throwSchedulerFailure()
      await commitReady()
      throwSchedulerFailure()
      // Abort may arrive while a tool or ordered commit awaits.

      if (signal.aborted) aborted = true
      await fillPool()
    }
  } catch (error: unknown) {
    schedulerFailure ??= { error }
    await Promise.allSettled(inFlight.values())
    throw schedulerFailure.error
  } finally {
    for (const throttle of throttles) throttle.close()
  }

  if (aborted || halt !== undefined) {
    // Started calls and accepted context settle first; every remaining model
    // call then receives an ordered synthetic result before the turn ends.
    for (const call of group.slice(started)) appendSkippedToolCall(session, turn, step, call.block)
    return { consumed: group.length, aborted, concluded, ...halt === undefined ? {} : { halt } }
  }
  /* v8 ignore next -- unreachable: a non-aborted group commits every started call */
  if (committed !== started) throw new Error('tool-call scheduler: uncommitted settled calls')
  return { consumed: started, aborted: false, concluded }
}

/** Append the durable call/result pair for a model call skipped after cancellation. */
function appendSkippedToolCall(session: Session, turn: number, step: number, block: ToolCallBlock): void {
  const callSeq = appendToolCall(session, turn, step, block)
  appendToolResult(session, turn, step, block, {
    content: [{ type: 'text', text: 'Error: tool call aborted before dispatch' }],
    isError: true,
    error: {
      message: 'tool call aborted before dispatch',
      info: { name: 'AbortError', code: TOOL_ABORTED_BEFORE_DISPATCH },
    },
  }, callSeq)
}

/** Append a started call and return the event seq that its result must cite. */
function appendToolCall(session: Session, turn: number, step: number, block: ToolCallBlock): SessionSeq {
  const event = session.append('tool/call', { turn, step, callId: block.id, name: block.name, arguments: block.arguments })
  return event.seq
}

/** Append a model-ordered result linked to its call event. */
function appendToolResult(
  session: Session,
  turn: number,
  step: number,
  block: ToolCallBlock,
  result: ToolExecutionResult,
  callSeq: SessionSeq,
): void {
  const message = createToolResultMessage({
    callId: block.id,
    content: result.content,
    isError: result.isError,
  })
  session.append('tool/result', {
    turn, step,
    message,
    ...result.error?.info ? { error: result.error.info } : {},
    // The tool's private presentation payload (e.g. a result-time diff),
    // persisted so a UI bridge reproduces the card on replay.
    ...result.meta !== undefined ? { meta: result.meta } : {},
  }, { surfaceOp: 'append', sourceEventSeqs: [callSeq] })
}
