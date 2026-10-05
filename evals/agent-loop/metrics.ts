/**
 * Loop-shape metrics the agent-loop eval extracts from one sample's event
 * stream: how the agent oriented itself, split edits from checks, and
 * verified its work. Pure functions over normalized events, so they are
 * tested on synthetic streams. Definitions are in evals/README.md.
 */

/** One tool call; `step` is the zero-based model response that made it. */
export interface Call { tool: string; input: any; callId: string; step: number }
/** Steps, calls, and results in one shape, whichever agent produced the event stream. */
export interface Normalized {
  final: string
  steps: { usage?: Record<string, number> }[]
  calls: Call[]
  results: { callId: string; status: string; result: string }[]
}

/** Bake's headless `--json` stream: each `step_start` opens the step its following calls belong to. */
export function normalizeBake(events: any[]): Normalized {
  let step = -1
  const calls: Call[] = []
  for (const event of events) {
    if (event.type === 'status' && event.phase === 'step_start') step++
    else if (event.type === 'tool_call') calls.push({ ...event, step: Math.max(step, 0) })
  }
  return {
    final: events.findLast(event => event.type === 'final')?.text ?? '',
    steps: events.filter(event => event.type === 'status' && event.phase === 'step_end'),
    calls,
    results: events.filter(event => event.type === 'tool_result'),
  }
}

/** pi's `--mode json` stream: each assistant `message_end` is one model request, and the calls after it are its own. */
export function normalizePi(events: any[]): Normalized {
  const assistant = events.filter(event => event.type === 'message_end' && event.message?.role === 'assistant').map(event => event.message)
  const text = (content: unknown) => Array.isArray(content)
    ? content.filter((block: any) => block?.type === 'text').map((block: any) => block.text).join('') : String(content ?? '')
  let step = -1
  const calls: Call[] = []
  for (const event of events) {
    if (event.type === 'message_end' && event.message?.role === 'assistant') step++
    else if (event.type === 'tool_execution_start' && event.parentToolCallId === undefined) {
      calls.push({ tool: event.toolName, input: event.args, callId: event.toolCallId, step: Math.max(step, 0) })
    }
  }
  return {
    final: text(assistant.at(-1)?.content).trim(),
    steps: assistant.map(message => ({ usage: message.usage === undefined ? undefined : {
      inputTokens: message.usage.input ?? 0, outputTokens: message.usage.output ?? 0,
      cacheReadTokens: message.usage.cacheRead ?? 0, cacheWriteTokens: message.usage.cacheWrite ?? 0,
      totalTokens: message.usage.totalTokens ?? 0,
    } })),
    calls,
    results: events.filter(event => event.type === 'tool_execution_end' && event.parentToolCallId === undefined)
      .map(event => ({ callId: event.toolCallId, status: event.isError ? 'error' : 'ok', result: text(event.result?.content) })),
  }
}

const SHELL_TOOLS = new Set(['bash', 'pwsh'])
/** A shell command that writes a source file: a Python writer, an in-place sed or perl, or a redirect into src/. */
export const SHELL_EDIT = /python3?\b[\s\S]*(write_text|\.write\(|open\([^)]*['"]w)|sed\s+-[a-zA-Z]*i|perl\s+-[a-zA-Z]*i|>\s*src\//
const commandOf = (call: Call): string => SHELL_TOOLS.has(call.tool) && typeof call.input?.command === 'string' ? call.input.command : ''

/** A file change: `edit` or `write`, or a shell command that writes a source file. */
export const isEdit = (call: Call) => call.tool === 'edit' || call.tool === 'write' || SHELL_EDIT.test(commandOf(call))
/** A shell call that runs the scenario's check. */
export const isCheck = (call: Call, check: RegExp) => check.test(commandOf(call))
/** Shell calls that write a source file; the `shellEdits` counter. */
export const shellEdits = (calls: readonly Call[]) => calls.filter(call => call.tool === 'bash' && SHELL_EDIT.test(call.input?.command ?? '')).length

/**
 * Edit/check splits: a step whose calls are all edits, followed by a step whose
 * first call runs the check. Each costs a round trip the persona asks the model
 * to save by sending the check with the edit.
 */
export function editCheckSplits(calls: readonly Call[], check: RegExp): number {
  const bySteps = new Map<number, Call[]>()
  for (const call of calls) bySteps.set(call.step, [...bySteps.get(call.step) ?? [], call])
  let splits = 0
  for (const [step, own] of bySteps) {
    const next = bySteps.get(step + 1)
    if (own.every(call => isEdit(call) && !isCheck(call, check)) && next !== undefined && isCheck(next[0]!, check)) splits++
  }
  return splits
}

/** Whether a path names the workspace root: absent, `.`, `./`, or the workspace itself. */
const atRoot = (path: unknown, workspaces: readonly string[]) => path === undefined || path === null || path === ''
  || path === '.' || path === './' || workspaces.some(workspace => path === workspace || path === `${workspace}/`)

/** A shell command's words, with surrounding quotes stripped; enough for the simple commands these metrics classify. */
const wordsOf = (segment: string) => segment.split(/\s+/).filter(Boolean).map(word => word.replace(/^(['"])(.*)\1$/, '$2'))
/** A command's simple commands: split on `&&`, `||`, and `;`, each kept whole with its pipeline. */
const commandsOf = (command: string) => command.split(/&&|\|\||;/).map(part => part.trim()).filter(Boolean)
/** A leading `cd` only picks the directory the next command runs in. */
const isCd = (segment: string) => /^cd(\s|$)/.test(segment)

/** `find` options that filter by name or path, which make the call a search rather than a listing. */
const FIND_FILTERS = new Set(['-name', '-iname', '-path', '-ipath', '-wholename', '-iwholename', '-regex', '-iregex'])

/** Whether one pipeline stage only lists: `pwd`, or `ls`, `tree`, or an unfiltered `find`, at the workspace root. */
function listingSegment(segment: string, workspaces: readonly string[]): boolean {
  const [first, ...rest] = wordsOf(segment)
  const paths = rest.filter(word => !word.startsWith('-'))
  if (first === 'pwd') return true
  if (first === 'ls' || first === 'tree') return paths.length === 0 || paths.every(path => atRoot(path, workspaces))
  if (first === 'find') return !rest.some(word => FIND_FILTERS.has(word)) && atRoot(paths[0], workspaces)
  return false
}

/** Whether a shell command only orients: every stage other than a `cd` is a listing. */
function orientingCommand(command: string, workspaces: readonly string[]): boolean {
  const segments = command.split(/&&|\|\||;|\|/).map(segment => segment.trim()).filter(segment => segment !== '' && !isCd(segment))
  return segments.length > 0 && segments.every(segment => listingSegment(segment, workspaces))
}

/**
 * Whether a shell command reads a file: `cat`, `head`, `tail`, `less`, or
 * `sed -n` with a file operand, as the first stage of one of its pipelines.
 * A later stage reads its input, not a file.
 */
export function readsFile(command: string): boolean {
  return commandsOf(command).filter(part => !isCd(part)).some(part => {
    const [first, ...rest] = wordsOf(part.split('|')[0]!)
    const operands: string[] = []
    for (let index = 0; index < rest.length; index++) {
      const word = rest[index]!
      // `head -n 5` and `tail -c 100` take a value that is not a file.
      if ((first === 'head' || first === 'tail') && (word === '-n' || word === '-c')) { index++; continue }
      if (word.startsWith('-') && word !== '-') continue
      if (word.startsWith('>') || word.startsWith('<') || word === '2>&1') break
      operands.push(word)
    }
    if (first === 'cat' || first === 'head' || first === 'tail' || first === 'less') return operands.some(word => word !== '-')
    // `sed -n` takes its script first, so a file is a second operand.
    if (first === 'sed') return rest.includes('-n') && operands.length >= 2
    return false
  })
}

/** Whether a glob pattern lists rather than searches: `*` or `**`, or `*` or `*.ext` under an optional `**` directory prefix, with no name stem. */
export const isBareGlob = (pattern: unknown) => typeof pattern === 'string'
  && /^(?:\.\/)?(?:\*\*|(?:\*\*\/)?\*(?:\.(?:\w+|\{\w+(?:,\w+)*\}))?)$/.test(pattern)

/** Whether a call reads a file: the `read` tool, or a shell command that prints one. */
const isRead = (call: Call) => call.tool === 'read' || readsFile(commandOf(call))

/**
 * Orientation calls: `pwd`, `ls`, `tree`, or a `find` without a name or path
 * filter, at the workspace root and alone in their command but for `cd`; an
 * `ls` tool call at the root; or a `glob` at the root whose pattern names no
 * file stem. Counted before the first read, by the `read` tool or a shell
 * `cat`, `head`, `tail`, `less`, or `sed -n` (or anywhere, when the sample
 * never reads).
 */
export function orientationCalls(calls: readonly Call[], workspaces: readonly string[]): number {
  const firstRead = calls.findIndex(isRead)
  return calls.slice(0, firstRead === -1 ? calls.length : firstRead).filter(call => {
    if (call.tool === 'ls') return atRoot(call.input?.path, workspaces)
    if (call.tool === 'glob') return atRoot(call.input?.path, workspaces) && isBareGlob(call.input?.pattern)
    const command = commandOf(call)
    return command !== '' && orientingCommand(command, workspaces)
  }).length
}

/** Whether the check ran at all. */
export const ranCheck = (calls: readonly Call[], check: RegExp) => calls.some(call => isCheck(call, check))

/**
 * Whether the check ran after the last edit; a shell call that both edits and
 * checks counts. Null when the sample made no edit.
 */
export function verifiedBeforeFinal(calls: readonly Call[], check: RegExp): boolean | null {
  const lastEdit = calls.findLastIndex(isEdit)
  if (lastEdit === -1) return null
  return calls.some((call, index) => index >= lastEdit && isCheck(call, check))
}

/** Requests minus the scenario's floor, unclamped: a negative value means a sample beat the floor, so the floor is set too high. */
export const requestsOverFloor = (requests: number, floor: number) => requests - floor
/** Requests above the scenario's floor, never negative. */
export const excessRequests = (requests: number, floor: number) => Math.max(0, requestsOverFloor(requests, floor))

/** Shell calls started with `run_in_background`. */
export const backgroundStarts = (calls: readonly Call[]) => calls.filter(call => SHELL_TOOLS.has(call.tool) && call.input?.run_in_background === true).length

/** The error the llm-pi-ai whitespace-runaway guard raises when it aborts a tool call's stream. */
export const RUNAWAY_ABORT = /streamed \d+ whitespace characters after the arguments of tool call/
/** Whether any of a sample's texts (stderr, the final reply, unparsed output) carries the guard's abort. */
export const runawayAbort = (texts: readonly string[]) => texts.some(text => RUNAWAY_ABORT.test(text))

/** The subagent routing decisions a run's session events recorded. */
export function routingDecisions(events: readonly { type?: string; data?: any }[]) {
  return events.filter(event => event.type === 'subagent/routing-decision')
    .map(({ data }) => ({ source: data.source, model: data.route?.model ?? null, effort: data.route?.reasoningEffort ?? null,
      routerFallback: data.router?.fallback ?? null, routerStatus: data.router?.assessment?.status ?? null,
      routerReason: typeof data.router?.reason === 'string' ? data.router.reason.slice(0, 120) : null }))
}

/** Completed summarizing compactions, and compaction attempts that ended in an error, from session events. */
export function compactions(events: readonly { type?: string; data?: any }[]) {
  return {
    summaries: events.filter(event => event.type === 'compaction/summary').length,
    errors: events.filter(event => event.type === 'compaction/end' && typeof event.data?.error === 'string').length,
  }
}
