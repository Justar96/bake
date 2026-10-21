/**
 * The six baseline workloads and their logical history, independent of any
 * session format.
 *
 * The generator follows Bake 0.3's terminal performance fixtures
 * (`apps/tui/packages/app/performance/history.ts` and `report.ts` at v0.3.8)
 * text for text, so a 0.3 Session log and a Pi session seeded from the same
 * workload hold the same prompts, reasoning, answers, tool calls, results, and
 * history markers. It is copied rather than imported so it outlives the
 * TypeScript agent.
 */

/** Fixed epoch, in milliseconds, of every synthetic timestamp. */
export const TIME = 1_700_000_000_000
const ANSWER_SECTION_BYTES = 4096
const ANSWER_TEXT = 'Validate ordering, cancellation, and resource cleanup. '
const REASONING_TEXT = 'Compare the synthetic implementation and its tests. '
const TOOL_TEXT = 'Synthetic tool output: observed value matches expected value.\n'.repeat(128)
/** The historical tool's name and arguments, in every format. */
export const TOOL_NAME = 'synthetic_tool'
export const TOOL_ARGUMENTS = '{"path":"src/example.ts"}'

/** Tool density and assistant message size, independent of the turn count. */
export interface WorkloadSpec {
  /** Completed historical turns. */
  readonly turns: number
  /** One tool-bearing turn every this many turns; defaults to four. */
  readonly toolEvery?: number
  /** Calls and results in each tool-bearing turn; defaults to one. */
  readonly toolsPerTurn?: number
  /** ASCII bytes per assistant answer, in 4096-byte sections; omitted keeps ordinary prose. */
  readonly assistantTextBytes?: number
}

/** The 0.3 driver's workloads, by name. */
export const WORKLOADS = {
  fresh: { turns: 0 },
  small: { turns: 50 },
  typical: { turns: 500 },
  tail: { turns: 2000 },
  tools: { turns: 500, toolEvery: 1, toolsPerTurn: 4 },
  'large-output': { turns: 12, assistantTextBytes: 64 * 1024 },
} as const satisfies Record<string, WorkloadSpec>

export type WorkloadName = keyof typeof WORKLOADS

/** Whether `name` names one of {@link WORKLOADS}. */
export function isWorkloadName(name: string): name is WorkloadName {
  return Object.hasOwn(WORKLOADS, name)
}

/** One historical tool call and its result. */
export interface HistoryCall {
  /** The call id, shared by the call and its result. */
  readonly id: string
  /** The 0.3 result message id. */
  readonly messageId: string
  /** The result text, ending in a history marker on the session's final call. */
  readonly output: string
}

/** One completed turn: a prompt, an answer with reasoning, and its tool calls. */
export interface HistoryTurn {
  readonly turn: number
  readonly userText: string
  readonly reasoning: string
  readonly answer: string
  readonly calls: readonly HistoryCall[]
}

/** Format-independent sizes of a workload's history. */
export interface HistoryDimensions {
  turns: number
  toolCount: number
  assistantMarkerCount: number
  /** Markers resume must show: every answer section, and the final tool result's. */
  historyMarkerCount: number
  userTextBytes: number
  assistantTextBytes: number
  reasoningBytes: number
  toolOutputBytes: number
  maxAssistantTextBytes: number
}

/** A workload's turns and dimensions. */
export interface History {
  readonly turns: readonly HistoryTurn[]
  readonly dimensions: HistoryDimensions
}

/** The history marker numbered `n`, as resume draws it. */
export function marker(n: number): string {
  return `H${String(n).padStart(5, '0')}_END`
}

/** Every marker a resumed `history` must show, in order. */
export function expectedMarkers(dimensions: HistoryDimensions): string[] {
  return Array.from({ length: dimensions.historyMarkerCount }, (_, index) => marker(index + 1))
}

/**
 * Build a workload's completed turns, as the 0.3 fixture author does.
 * @param spec - the turn count, tool density, and answer size.
 * @returns the turns and their dimensions.
 */
export function history(spec: WorkloadSpec): History {
  const sections = spec.assistantTextBytes === undefined ? 1 : spec.assistantTextBytes / ANSWER_SECTION_BYTES
  if (!Number.isSafeInteger(sections) || sections < 1) throw new Error('Assistant text size must be a positive multiple of 4096 bytes')
  const toolEvery = spec.toolEvery ?? 4
  const toolsPerTurn = spec.toolsPerTurn ?? 1
  const dimensions: HistoryDimensions = {
    turns: spec.turns, toolCount: 0, assistantMarkerCount: spec.turns * sections, historyMarkerCount: spec.turns * sections,
    userTextBytes: 0, assistantTextBytes: 0, reasoningBytes: 0, toolOutputBytes: 0, maxAssistantTextBytes: 0,
  }
  const turns: HistoryTurn[] = []
  for (let turn = 1; turn <= spec.turns; turn++) {
    const userText = `Synthetic review ${turn}. ` + '检查边界和顺序。 '.repeat(8)
    dimensions.userTextBytes += Buffer.byteLength(userText)
    const answer = Array.from({ length: sections }, (_, index) => {
      const end = `\n${marker((turn - 1) * sections + index + 1)}`
      return spec.assistantTextBytes === undefined ? ANSWER_TEXT.repeat(12) + end
        : ANSWER_TEXT.repeat(Math.ceil(ANSWER_SECTION_BYTES / ANSWER_TEXT.length)).slice(0, ANSWER_SECTION_BYTES - end.length - 1) + end + '\n'
    }).join('')
    const reasoning = REASONING_TEXT.repeat(6)
    const answerBytes = Buffer.byteLength(answer)
    dimensions.assistantTextBytes += answerBytes
    dimensions.maxAssistantTextBytes = Math.max(dimensions.maxAssistantTextBytes, answerBytes)
    dimensions.reasoningBytes += Buffer.byteLength(reasoning)
    const calls: HistoryCall[] = []
    if (turn % toolEvery === 0) {
      for (let index = 0; index < toolsPerTurn; index++) {
        const suffix = toolsPerTurn === 1 ? '' : `-${index + 1}`
        // The final tool body follows the last answer, so its marker ends the history.
        const last = turn === spec.turns && index === toolsPerTurn - 1
        const output = last ? TOOL_TEXT + `${marker(++dimensions.historyMarkerCount)}\n` : TOOL_TEXT
        dimensions.toolCount++
        dimensions.toolOutputBytes += Buffer.byteLength(output)
        calls.push({ id: `perf-call-${turn}${suffix}`, messageId: `perf-tool-${turn}${suffix}`, output })
      }
    }
    turns.push({ turn, userText, reasoning, answer, calls })
  }
  return { turns, dimensions }
}
