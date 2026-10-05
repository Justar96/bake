/**
 * @deepseek-ai/dsh-headless — one-shot direct Agent driver. The bundle patch
 * rides over dsh-base without Host, HTTP, or browser plugins; this runner
 * creates one Agent through the core registry (or adopts the exact Session a
 * `--resume` names), drives the task to quiescence, streams provider
 * reasoning to stderr, flushes its Session, prints the final assistant text to
 * stdout, and exits. With `--json` it projects the run as newline-delimited
 * events instead of the final text.
 *
 * @module @deepseek-ai/dsh-headless
 */

import { randomUUID } from 'node:crypto'
import type { Context } from '@deepseek-ai/cordis'
import z from '@deepseek-ai/schemastery'
import { brandString } from '@deepseek-ai/dsh-brand'
import { installModelSelection } from '@deepseek-ai/dsh-agent'
import type { Agent, ModelSelectionRef } from '@deepseek-ai/dsh-agent'
import { NO_DEFAULT_MODEL_MESSAGE } from '@deepseek-ai/dsh-agent-default-model'
import type {} from '@deepseek-ai/dsh-agent-loop'
import type {} from '@deepseek-ai/dsh-fs'
import type {} from '@deepseek-ai/dsh-jobs'
import { createUserMessage } from '@deepseek-ai/dsh-llm'
import { assertNever } from '@deepseek-ai/dsh-util-values'
import { SessionSeq } from '@deepseek-ai/dsh-session'
import type { Session, SessionEvent, SessionId, SessionLogOffset } from '@deepseek-ai/dsh-session'
import { SessionAlreadyOwnedError } from '@deepseek-ai/dsh-session-persistence'
import { SessionQueryError } from '@deepseek-ai/dsh-session-query'
// The cmdline import also carries the Context merge for the appExit host value.
import { SESSION_IN_USE_EXIT, SessionInUseError } from '@deepseek-ai/dsh-cmdline'
// Empty type imports carry the loader Context merge for the settlement await
// and the sessionQuery Context merge for exact Session adoption.
import type {} from '@deepseek-ai/cordis-plugin-loader'
import type {} from '@deepseek-ai/dsh-session-query'
import { internals } from './runner-internals.ts'
import { projectJsonRun, boundJsonLine } from './json-stream.ts'

/** Stable Cordis plugin name. */
export const name = 'headless-runner'

/** Core services required before the one-shot turn can start. */
export const inject = ['agentDefaultModel', 'agents', 'sessions']

/** Plugin config: the task and run options resolved from this app's injected provider service. */
export interface Config {
  /** The prompt text for the single run; absent when the task arrives on stdin. */
  task?: string
  /** Exact Session identity to adopt; absent for a fresh random identity. An id with no stored Session fails. */
  sessionId?: string
  /** Whether stdout carries the machine-readable event stream instead of final text. */
  json?: boolean
  /**
   * Longest the run waits, in milliseconds, for background jobs its Agent still
   * owns once its turn ends, and for the turns their completions open. A job
   * that never ends, such as a dev server, is then stopped with the run. 0 does
   * not wait. Defaults to 600000 (10 minutes).
   */
  jobWaitMs?: number
}

/** Default {@link Config.jobWaitMs}: as long as one `job_output` wait could once hold a turn. */
const JOB_WAIT_MS = 600_000

export const Config: z<Config> = z.object({
  task: z.string(),
  sessionId: z.string(),
  json: z.boolean(),
  jobWaitMs: z.number().min(0).default(JOB_WAIT_MS),
})

/**
 * What follows the Session id when another process has the `--resume` Session
 * open. It is the terminal profile's English launch refusal, so both profiles
 * print the same line; the PTY `session-in-use` scenario compares the two.
 */
const SESSION_IN_USE_REFUSAL = 'open in another Bake process; close it there and run this command again, or leave out --resume to start a new session'

/** Outcome of one owned run interval. */
interface RunOutcome {
  text: string
  reason: SessionEvent<'turn/end'>['data']['reason'] | undefined
}

/** Process-facing effects of one run: output streams plus the launcher's bounded exit request. */
interface HeadlessIo {
  stdout: { write(chunk: string): unknown }
  stderr: { write(chunk: string): unknown }
  /** Request process exit with `code` after the tree disposes. */
  exit(code: number): void
}

/** Aggregate the last assistant text and turn outcome in one owned interval. */
function summarize(session: Session, firstSeq: SessionLogOffset): RunOutcome {
  let started = false
  let text = ''
  let reason: SessionEvent<'turn/end'>['data']['reason'] | undefined
  const length = session.seq
  for (let seq = firstSeq; seq < length; seq++) {
    // oxlint-disable-next-line typescript/no-deprecated -- Existing Session history read; migration deferred.
    const event = session.eventAt(SessionSeq(seq))
    if (event === undefined) {
      throw new Error(`headless summary cannot read seq ${String(seq)} below captured length ${String(length)}`)
    }
    if (event.type === 'turn/start') {
      started = true
      continue
    }
    if (!started) continue
    if (event.type === 'assistant/message') {
      const joined = event.data.message.content
        .filter(block => block.type === 'text')
        .map(block => block.text)
        .join('')
      if (joined !== '') text = joined
    }
    if (event.type === 'turn/end') reason = event.data.reason
  }
  return { text, reason }
}

/**
 * Project provider-reported reasoning from one owned run to stderr as it is
 * streamed, while keeping final outcome derivation on the durable log.
 * @param ctx - plugin context carrying the live Assistant frame feed.
 * @param agent - the exact Agent whose reasoning belongs to this invocation.
 * @param stderr - progress output sink.
 * @returns a disposer that also terminates an unterminated reasoning line.
 */
function streamReasoning(
  ctx: Context,
  agent: Agent,
  stderr: HeadlessIo['stderr'],
): () => void {
  let open = false
  let endsWithNewline = true
  const close = (): void => {
    if (!open) return
    if (!endsWithNewline) stderr.write('\n')
    open = false
    endsWithNewline = true
  }
  const dispose = ctx.on('agent/assistant-stream', ({ agent: subject, frame }) => {
    if (subject !== agent) return
    if (frame.type === 'start') {
      close()
      return
    }
    if (frame.type === 'end') {
      close()
      return
    }
    const chunk = frame.chunk
    switch (chunk.type) {
      case 'reasoning-delta':
        if (chunk.text === '') return
        if (!open) {
          stderr.write('dsh: reasoning:\n')
          open = true
        }
        stderr.write(chunk.text)
        endsWithNewline = chunk.text.endsWith('\n')
        return
      case 'block-start':
        if (chunk.blockType !== 'reasoning') close()
        return
      case 'block-end':
        if (chunk.block.type !== 'reasoning') close()
        return
      case 'usage':
        return
      case 'text-delta':
      case 'tool-call-delta':
      case 'finish':
        close()
        return
      /* v8 ignore next -- closed-union exhaustiveness guard */
      default:
        return assertNever(chunk, 'headless reasoning stream')
    }
  })
  return () => {
    dispose()
    close()
  }
}

/** The Session facts that decide whether the runner may drive it directly. */
interface AdoptableHeader {
  cwd?: string | undefined
  origin?: 'subagent' | undefined
  parentSession?: SessionId | undefined
  agentPreset?: string | undefined
}

/** Iterate a live Session's durable events in order. */
function* liveEvents(session: Session): Generator<SessionEvent> {
  const length = session.seq
  for (let seq = 0; seq < length; seq++) {
    // oxlint-disable-next-line typescript/no-deprecated -- Existing Session history read; migration deferred.
    const event = session.eventAt(SessionSeq(seq))
    if (event === undefined) {
      throw new Error(`headless adoption cannot read seq ${String(seq)} below captured length ${String(length)}`)
    }
    yield event
  }
}

/**
 * The preset a Session currently runs under: its creation header advanced by
 * the last `agent-preset/selected` event. The header is only a creation fact;
 * the presets plugin reconstructs a session's composition from the projection.
 */
function currentPreset(header: AdoptableHeader, events: Iterable<SessionEvent>, sessionId: SessionId): string | undefined {
  let preset = header.agentPreset
  for (const event of events) {
    // Owned by dsh-agent-presets, which this bundle does not compose, so the
    // event is read structurally rather than through its module augmentation.
    const candidate = event as unknown as { type: string; data?: { agentPreset?: unknown } }
    if (candidate.type !== 'agent-preset/selected') continue
    const selected = candidate.data?.agentPreset
    // A corrupt record must not read as "no preset": that would let the run
    // continue under this bundle's composition instead of the recorded one.
    if (typeof selected !== 'string' || selected === '') {
      throw new Error(`session "${sessionId}" records a malformed agent-preset/selected event and cannot be adopted`)
    }
    preset = selected
  }
  return preset
}

/** Reject a Session the one-shot runner must not adopt. */
function assertAdoptable(header: AdoptableHeader, events: Iterable<SessionEvent>, sessionId: SessionId, cwd: string): void {
  const preset = currentPreset(header, events, sessionId)
  if (preset !== undefined) {
    // This bundle composes no preset roster, so resuming the session here would
    // silently run it under the headless tools and prompts instead of the
    // composition its log records.
    throw new Error(
      `session "${sessionId}" runs under agent preset "${preset}", which the one-shot runner does not compose`,
    )
  }
  if (header.origin === 'subagent' || header.parentSession !== undefined) {
    throw new Error(`session "${sessionId}" is a subagent or forked session and cannot be driven directly`)
  }
  if (header.cwd === undefined) {
    throw new Error(`session "${sessionId}" recorded no working directory, so it cannot be adopted`)
  }
  if (header.cwd !== cwd) {
    throw new Error(`session "${sessionId}" was recorded in "${header.cwd}", not "${cwd}"`)
  }
}

/**
 * Resolve the Agent for one run: adopt the persisted Session with the requested
 * id. The identity must already exist, and no Agent may be live under it; a
 * first round omits the option instead, so a typo cannot pass as a brand-new
 * conversation.
 * @param ctx - plugin context carrying the Session query service.
 * @param agents - the core Agent registry.
 * @param sessionId - exact Session identity to adopt.
 * @param agentOptions - provider/model pair for this run.
 * @param setup - per-Agent scope setup installing the model selection.
 * @param cwd - working directory resolved in the mounted filesystem.
 * @returns the resumed Agent.
 */
async function resolveAgent(
  ctx: Context,
  agents: Context['agents'],
  sessionId: SessionId,
  agentOptions: { provider: string; model: string } | undefined,
  setup: (agentCtx: Context, agent: Agent) => void,
  cwd: string,
): Promise<Agent> {
  // Resuming promises the caller a log a later process can continue. Without a
  // durable log the run would succeed, print the id, and still lose the whole
  // history at exit, so a miscomposed profile fails loud before the resume.
  if (ctx.get('sessionPersistence') === undefined) {
    throw new Error('headless --resume requires the sessionPersistence service; the Session would not survive this process')
  }
  // A later process holds no live Agent and has to find the id through the
  // query service, so every --resume run requires it.
  const query = ctx.get('sessionQuery')
  if (query === undefined) {
    throw new Error('headless --resume requires the sessionQuery service; dsh-base provides it')
  }
  const live = agents.get(sessionId)
  if (live !== undefined) {
    // A live Agent already has an owner that may still drive it, and `whenIdle`
    // is not a single-message signal: folding its next interval into this run
    // would mix that owner's events — even its final answer — into the stream.
    // The runner cannot claim an exclusive interval over an Agent it did not
    // create, so it refuses the identity; the adoptability rules run first so a
    // real mismatch is named instead of the generic refusal.
    assertAdoptable(live.session.header, liveEvents(live.session), sessionId, cwd)
    throw new Error(`session "${sessionId}" is live in this process, so the one-shot runner cannot own an exclusive run interval`)
  }
  try {
    using observation = await query.observeSession(sessionId)
    assertAdoptable(observation.header, observation.events, sessionId, cwd)
    const { agent } = await agents.resume({ resumeSessionId: sessionId, ...agentOptions === undefined ? {} : { agentOptions }, setup })
    // The observation is a snapshot: another writer may have appended a preset
    // selection before this process took the write lease. Re-check the log
    // resume actually attached, now that no other process can append.
    assertAdoptable(agent.session.header, liveEvents(agent.session), sessionId, cwd)
    return agent
  } catch (error: unknown) {
    // No Agent of this process holds the id (checked above), and nothing else
    // this composition mounts opens a Session for write, so a write refusal
    // here is the kernel lock another process holds.
    if (error instanceof SessionAlreadyOwnedError) throw new SessionInUseError(`${sessionId}: ${SESSION_IN_USE_REFUSAL}`, error)
    if (!(error instanceof SessionQueryError) || error.code !== 'SESSION_QUERY_SESSION_NOT_FOUND') throw error
    // --resume adopts a conversation that already exists; starting a new
    // one is the no-id path, which generates its own identity and reports it in
    // the `session` event. Creating the requested id here would turn a typo
    // into a brand-new empty history the caller believes it is continuing.
    throw new Error(`session "${sessionId}" does not exist; omit --resume to start a new Session`)
  }
}

/**
 * Report a run the process stopped, by a signal or by disposing the Agent,
 * before its turn finished. The stop is not a failure of the run, so stdout
 * carries no `error` or `final`; the turn's end and its reason are already in
 * the JSON stream when the turn started. Disposal closes the Session, and
 * close drains its log, so the next process can continue it. The exit request
 * is ignored while a signal's shutdown runs, which keeps that signal's code.
 */
function stopped(io: HeadlessIo, sessionId: SessionId | undefined, resumable: boolean): void {
  io.stderr.write(sessionId !== undefined && resumable
    ? `dsh: stopped before the task finished; continue it with --resume ${sessionId}\n`
    : 'dsh: stopped before the task finished\n')
  io.exit(1)
}

/** Whether the run's turn ended because its Agent was disposed. */
function endedByDisposal(outcome: RunOutcome): boolean {
  return outcome.reason?.kind === 'aborted' && outcome.reason.reason.kind === 'disposed'
}

/**
 * Report an unexpected direct-driver failure and request a failing exit: the
 * in-use status for a `--resume` Session another process has open, 1 otherwise.
 */
function fail(io: HeadlessIo, error: unknown, json: boolean): void {
  const message = error instanceof Error ? error.message : String(error)
  if (json) io.stdout.write(`${boundJsonLine({ type: 'error', message })}\n`)
  io.stderr.write(`dsh: ${message}\n`)
  io.exit(error instanceof SessionInUseError ? SESSION_IN_USE_EXIT : 1)
}

/**
 * Run one task through one Agent and request process exit.
 * @param ctx - plugin context carrying the Agent, default model, Session, and launcher IO services.
 * @param config - task, optional exact Session identity, and output mode.
 * @param io - process-facing effects.
 */
/** How often a run left waiting on background jobs checks whether they have settled. */
const JOB_POLL_MS = 250

/**
 * Wait out the Agent's background jobs before the run reports.
 *
 * A turn may end while its jobs run, trusting their completion notices to open
 * the turns that use the results. The process must outlive those jobs and
 * turns, or disposal would cancel the work the answer depends on. The registry
 * is polled, not waited on: a registry wait marks the job reported, which
 * suppresses the very notice that reopens the turn.
 *
 * The wait is bounded, since a job may never end on its own. A turn a
 * completion opened still runs to its end past the limit; no later job is
 * waited on.
 * @param ctx - the runner's context, whose `jobs` service may be absent.
 * @param agent - the Agent this run owns.
 * @param stopping - aborted when the process is asked to stop.
 * @param limitMs - longest wait for jobs, in milliseconds.
 * @returns how many jobs were still running when the limit ran out.
 */
async function settleJobs(ctx: Context, agent: Agent, stopping: AbortSignal, limitMs: number): Promise<number> {
  const jobs = ctx.get('jobs')
  if (jobs === undefined) return 0
  const running = (): number => jobs.list(agent).filter(job => job.status === 'running' || job.status === 'stopping').length
  const deadline = Date.now() + limitMs
  while (running() > 0 && !stopping.aborted) {
    while (running() > 0 && !stopping.aborted) {
      const left = deadline - Date.now()
      if (left <= 0) return running()
      await new Promise<void>((resolve) => {
        const done = (): void => {
          clearTimeout(timer)
          stopping.removeEventListener('abort', done)
          resolve()
        }
        const timer = setTimeout(done, Math.min(JOB_POLL_MS, left))
        stopping.addEventListener('abort', done, { once: true })
      })
    }
    // A settlement opens its notice's turn synchronously, so the Agent is
    // already busy here when one was woken; that turn may start more jobs.
    await agent.whenIdle()
  }
  return 0
}

async function run(ctx: Context, config: Config, io: HeadlessIo, stopping: AbortSignal): Promise<void> {
  // Loader siblings mount concurrently. Await the complete application before
  // creating an Agent so its scoped tools and adapters are not half-composed.
  await ctx.get('loader')?.await()
  const agents = ctx.get('agents')
  const defaultModel = ctx.get('agentDefaultModel')
  const sessions = ctx.get('sessions')
  // Early process shutdown can dispose the tree while settlement is pending.
  if (agents === undefined || defaultModel === undefined || sessions === undefined) return

  // A Cordis overlay sets the row directly and bypasses the CLI trim check, so
  // the same public setting must fail here rather than become a blank identity.
  if (config.sessionId !== undefined && config.sessionId.trim() === '') {
    throw new Error('headless-runner: sessionId must not be blank')
  }

  const task = config.task === undefined || config.task === '-'
    ? await internals.readStdin()
    : config.task
  if (task.trim() === '') {
    throw new Error('a task is required, for example: dsh --profile headless "run the tests"')
  }

  // No provider is the default. A new run needs the selection a user saved;
  // a resumed one without it continues on the model its log last requested.
  const selection = defaultModel.currentSelection()
  if (selection === undefined && config.sessionId === undefined) throw new Error(NO_DEFAULT_MODEL_MESSAGE)
  const agentOptions = selection === undefined ? undefined : { provider: selection.provider, model: selection.model }
  // This bundle composes no preset roster, so the model-facing rows sit in the
  // host plane and the agent reads them from the global layer. A deployment
  // that DOES configure one has to join it here first
  // (@deepseek-ai/dsh-agent-presets README, "Composing a child agent").
  const setup = (agentCtx: Context, agent: Agent): void => {
    const recorded = agent.session.requestHeader()?.config
    const current = selection ?? (recorded === undefined ? undefined : { provider: recorded.provider, model: recorded.model })
    if (current === undefined) throw new Error(NO_DEFAULT_MODEL_MESSAGE)
    const selected: ModelSelectionRef = { current, assembled: undefined }
    installModelSelection(agentCtx, selected)
  }
  const sessionId = brandString<SessionId>(config.sessionId ?? `session-${randomUUID()}`)
  const fs = ctx.get('fs')
  const cwd = fs === undefined ? process.cwd() : fs.processPath(await fs.resolve('.'))
  const agent = config.sessionId === undefined
    ? (await agents.create({
      sessionId,
      meta: { cwd },
      ...agentOptions === undefined ? {} : { agentOptions },
      setup,
    })).agent
    : await resolveAgent(ctx, agents, sessionId, agentOptions, setup, cwd)
  await agent.whenIdle()
  if (config.sessionId !== undefined) {
    // The resume-time check read a snapshot; an overlay can still append a
    // preset selection between it and the interval this run now owns, so
    // re-read the log the runner holds before submitting the task.
    assertAdoptable(agent.session.header, liveEvents(agent.session), sessionId, cwd)
  }
  const firstSeq = agent.session.seq
  const projection = config.json === true ? projectJsonRun(ctx, agent, io.stdout, { cwd }) : undefined
  const stopReasoning = projection === undefined ? streamReasoning(ctx, agent, io.stderr) : undefined
  try {
    try {
      agent.followup(createUserMessage({
        content: [{ type: 'text', text: task }],
        source: { kind: 'user' },
      }))
      await agent.whenIdle()
      const waitMs = config.jobWaitMs ?? JOB_WAIT_MS
      const left = await settleJobs(ctx, agent, stopping, waitMs)
      if (left > 0) {
        io.stderr.write(`dsh: warning: ${String(left)} background job${left === 1 ? '' : 's'} still running after ${String(waitMs / 1000)} s; stopping ${left === 1 ? 'it' : 'them'} with the run\n`)
      }
    } finally {
      stopReasoning?.()
    }
    // The in-memory log stays readable after disposal, so an answer the turn
    // finished before a stop is still delivered.
    const outcome = summarize(agent.session, firstSeq)
    // A turn that already ended for its own reason keeps that outcome.
    if (endedByDisposal(outcome) || (stopping.aborted && outcome.reason === undefined)) {
      stopped(io, sessionId, ctx.get('sessionPersistence') !== undefined)
      return
    }
    // Disposal flushes the log by closing the Session, which can leave the
    // store before a flush here reaches it.
    if (!stopping.aborted) await sessions.flush(agent.session)
    if (projection === undefined) io.stdout.write(outcome.text + '\n')
    else projection.finish(outcome.text)
    if (outcome.reason?.kind === 'error') {
      io.stderr.write(`dsh: ${outcome.reason.error.code}: ${outcome.reason.error.message}\n`)
    }
    io.exit(outcome.reason?.kind === 'completed' ? 0 : 1)
  } finally {
    projection?.dispose()
  }
}

/**
 * Mount the one-shot direct driver.
 * @param ctx - plugin context carrying core services and the launcher-provided exit request.
 * @param config - validated task and run options.
 */
export function apply(ctx: Context, config: Config): void {
  // Read through the global service store, not the property proxy: appExit is
  // an optional host value, never an injected dependency.
  const exit = ctx.get('appExit')
  if (exit === undefined) {
    throw new Error('headless-runner: the launcher must provide ctx.appExit before the tree mounts')
  }
  const io: HeadlessIo = { stdout: internals.stdout, stderr: internals.stderr, exit }
  // Aborted when the launcher starts shutting the app down, or when this
  // plugin unloads. `app/shutdown` runs before the tree disposes, so the run
  // sees it before the Agent's disposal resumes it.
  const stopping = new AbortController()
  ctx.on('app/shutdown', () => { stopping.abort() })
  ctx.effect(() => () => { stopping.abort() }, 'headless-runner stop')
  void run(ctx, config, io, stopping.signal).catch((error: unknown) => {
    // A failure once shutdown began is its consequence: the Session closed, or
    // the Agent could not start, while the process was stopping.
    if (stopping.signal.aborted) stopped(io, undefined, false)
    else fail(io, error, config.json === true)
  })
}
