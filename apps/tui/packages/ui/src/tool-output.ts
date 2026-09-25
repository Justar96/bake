/** Language hints and conservative colour for tool output. Source text stays in the log. */
import stripAnsi from 'strip-ansi'
import { PALETTE } from './palette.ts'
import type { CardLine } from './rows.ts'
import type { Span } from './present.ts'

/** Strip terminal escape sequences and make remaining controls visible before measuring text. */
export const toolText = (text: string): string => stripAnsi(text).replace(/\r\n?/g, '\n').replace(/\t/g, '    ')
  .replace(/[\x00-\x08\x0b\x0c\x0e-\x1f\x7f-\x9f]/g, char => `\\x${char.charCodeAt(0).toString(16).padStart(2, '0')}`)

/** Infer only valid structured JSON. Command stdout is not shell source. */
function jsonOutput(text: string): boolean {
  const value = (source: string): boolean => {
    if (!/^\s*[\[{]/.test(source)) return false
    try { JSON.parse(source); return true } catch { return false }
  }
  // Inference is optional. Very large output retains its readable plain form.
  if (text.length > 262_144) return false
  return value(text) || text.trim().split('\n').every(line => value(line))
}

/** Card lines preserving whitespace and carrying an explicit or inferred grammar. */
export function outputLines(raw: string, language?: string): readonly CardLine[] {
  const text = toolText(raw)
  if (text === '') return []
  const source = language ?? (jsonOutput(text) ? 'json' : undefined)
  const lines = text.split('\n')
  if (lines.at(-1) === '') lines.pop()
  return lines.map((text, index) => ({ text,
    ...source === undefined ? {} : { source, ...index === 0 ? { codeStart: true } : {} },
  }))
}

/**
 * Colour explicit diagnostic labels, URLs, and path-shaped tokens in plain
 * output. Ordinary words and numbers remain unstyled. Labels keep their text
 * under NO_COLOR. Syntax-highlighted source bypasses these heuristics.
 */
export function outputSpans(text: string): readonly Span[] | undefined {
  const spans: Span[] = []
  const pattern = /https?:\/\/[^\s<>"']+|(?:\b(?:[A-Za-z]:)?[\w.@~-]+|\.{1,2}|~)?[\\/][\w.@~+\-/\\]+(?::\d+(?::\d+)?)?|\b[\w@-]+\.(?:tsx?|jsx?|mjs|cjs|jsonc?|py|rs|go|ya?ml|toml|md|sh|css|html|txt|log)(?::\d+(?::\d+)?)?|\b(?:ERROR|Error|error|FATAL|Fatal|fatal|FAIL(?:ED)?|WARN(?:ING)?|Warning|warning|PASS(?:ED)?|SUCCESS|INFO|DEBUG)\b/g
  let offset = 0
  for (const match of text.matchAll(pattern)) {
    const word = match[0]
    const start = match.index
    const label = /^(?:error|fatal|fail(?:ed)?|warn(?:ing)?|pass(?:ed)?|success|info|debug)$/i.test(word)
    // Severity is a leading field, optionally after a timestamp or brackets.
    // A sentence mentioning an error does not claim a new failure.
    if (label && !/^[\s\dT:.Z+\-/\[\]()]*$/.test(text.slice(0, start))) continue
    if (start > offset) spans.push({ length: start - offset, tone: 'plain' })
    const upper = word.toUpperCase()
    spans.push({ length: word.length, tone: 'plain', color: !label ? PALETTE.reference
      : /^(ERROR|FATAL|FAIL)/.test(upper) ? PALETTE.failed
        : upper.startsWith('WARN') ? PALETTE.waiting
          : /^(PASS|SUCCESS)/.test(upper) ? PALETTE.done : PALETTE.reference })
    offset = start + word.length
  }
  if (spans.length === 0) return undefined
  if (offset < text.length) spans.push({ length: text.length - offset, tone: 'plain' })
  return spans
}
