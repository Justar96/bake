/** Shared code-mode fixtures: the presentation bound, event builder, and a `run_code` presenter shaped like the real one. */
import type { SessionEvent } from '@deepseek-ai/dsh-session'
import type { ToolLookup } from '../../src/cards.ts'
import { dictionaries } from '../../src/copy.ts'
import { SCRIPT_TOOL, type ResultBound } from '../../src/present.ts'

export const copy = dictionaries.en
export const bound: ResultBound = {
  lines: 4, unit: copy.cardLines, single: copy.cardLine, more: copy.moreLines, earlier: copy.earlierCalls,
  script: copy.scriptLabel, scriptOutput: copy.scriptOutput, scriptError: copy.scriptError,
  call: copy.scriptCall, calls: copy.scriptCalls, moreCalls: copy.moreCalls, failures: copy.summaryFailures,
}
export const event = (type: string, data: unknown): SessionEvent => ({ type, data }) as SessionEvent

/** Tool lookup for `projector`: only `run_code` has a presenter, which fences its source as TypeScript. */
export const codeModeTools: ToolLookup = name => name === SCRIPT_TOOL ? {
  presentCall: (value: unknown) => {
    const args = value as { code: string, description: string }
    return { card: 'generic', kind: 'execute', title: args.description, content: [{ type: 'text', text: `\`\`\`typescript\n${args.code}\n\`\`\`` }] }
  },
} : undefined
