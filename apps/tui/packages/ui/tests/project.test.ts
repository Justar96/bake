/**
 * Projection behavior. Runs under `bun test` because the module under test is
 * pure — no harness, no Node built-ins, no clock.
 */

import { describe, expect, it } from 'bun:test'
import type { SessionEvent } from '@deepseek-ai/dsh-session'
import stringWidth from 'string-width'
import {
  announcedCalls, argumentsTitle, HEADLINE_CELLS, outputRate, project, projector, type Projector,
} from '../src/project.ts'
import { PENDING_ARGUMENTS, present, RAW_LINE_CELLS } from '../src/present.ts'
import { dictionaries } from '../src/copy.ts'
import { formatRow } from '../src/plain.ts'

/** Build a session event literal without restating the durable envelope. */
const event = (partial: unknown): SessionEvent => partial as SessionEvent

/** A projector whose lookup finds no tool. Every call keeps its raw arguments. */
const bare = (): Projector => projector(dictionaries.en, () => undefined)

describe('project', () => {
  it('renders a human prompt as a user row', () => {
    expect(project(event({
      type: 'user/message',
      data: { source: { kind: 'user' }, content: [{ type: 'text', text: 'run the tests' }] },
    }), bare())).toEqual([{ kind: 'user', text: 'run the tests' }])
  })

  it('drops synthetic context injected by the loop', () => {
    expect(project(event({
      type: 'user/message',
      data: { source: { kind: 'plugin' }, content: [{ type: 'text', text: 'AGENTS.md changed' }] },
    }), bare())).toEqual([])
  })

  it('marks a call that asked to run in the background, and no other', () => {
    const call = (args: unknown) => project(event({ type: 'tool/call',
      data: { turn: 1, step: 1, callId: 'c1', name: 'bash', arguments: JSON.stringify(args) } }), bare())[0]
    expect(call({ command: 'npm run dev', run_in_background: true })).toMatchObject({ kind: 'tool-call', background: true })
    expect(call({ command: 'make', run_in_background: false })).not.toHaveProperty('background')
    expect(call({ command: 'echo run_in_background' })).not.toHaveProperty('background')
  })

  describe('a background job\'s completion notice', () => {
    const notice = (text: string, summary: string, plugin = 'tool-jobs') => project(event({
      type: 'user/message',
      data: { source: { kind: 'plugin', plugin, form: 'notice', summary }, content: [{ type: 'text', text }] },
    }), bare())
    const finished = (status: string) =>
      `background job bash-2 (bash: npm run dev) finished [status: ${status}]. Read its output with job_output.`

    it('closes the job with its outcome, the command\'s exit code deciding success', () => {
      expect(notice(finished('completed, exit code: 0'), 'ignored')).toEqual([{ kind: 'job-done', id: 'bash-2', tool: 'bash',
        label: 'npm run dev', outcome: 'done', status: 'finished · exit code: 0' }])
      expect(notice(finished('completed, exit code: 2'), '')[0]).toMatchObject({ outcome: 'failed', status: 'failed · exit code: 2' })
      expect(notice(finished('killed, signal: SIGTERM'), '')[0]).toMatchObject({ outcome: 'stopped', status: 'stopped · signal: SIGTERM' })
      expect(notice(finished('failed, spawn ENOENT'), '')[0]).toMatchObject({ outcome: 'failed', status: 'failed · spawn ENOENT' })
    })

    it('falls back to the summary when the text was cut, and drops anything else', () => {
      expect(notice('background job bash-2\n[notice truncated]', 'bash npm run dev [status: completed, exit code: 0]'))
        .toEqual([{ kind: 'job-done', tool: 'bash', label: 'npm run dev', outcome: 'done', status: 'finished · exit code: 0' }])
      expect(notice('background job bash-2\n[notice truncated]', 'bash npm run dev with a very long tail…')).toEqual([])
      expect(notice(finished('completed, exit code: 0'), 'x', 'repeat-tool-reminder')).toEqual([])
    })

    it('reads in plain text as a call naming its job', () => {
      expect(formatRow(notice(finished('completed, exit code: 0'), '')[0]!)).toBe('◌ bash [bash-2](npm run dev) finished · exit code: 0')
    })

    it('keeps controls in a label as data', () => {
      const [row] = notice('background job bash-1 (bash: echo \u001b[2Jhi) finished [status: completed, exit code: 0].', '')
      expect(row).toMatchObject({ label: 'echo hi' })
    })
  })

  it('separates reasoning from answer text, in order', () => {
    expect(project(event({
      type: 'assistant/message',
      data: { message: { content: [
        { type: 'reasoning', text: 'checking the config' },
        { type: 'text', text: 'done' },
      ] } },
    }), bare())).toEqual([
      { kind: 'reasoning', text: 'checking the config' },
      { kind: 'assistant', text: 'done' },
    ])
  })

  it('reads the calls a message announces, without committing them as rows', () => {
    const message = event({
      type: 'assistant/message',
      data: { message: { content: [
        { type: 'text', text: 'Reading both' },
        { type: 'tool-call', id: 'c1', name: 'read', arguments: '{"file_path":"a.ts"}' },
        { type: 'tool-call', id: 'c2', name: 'read', arguments: '{"file_path":"b.ts"}' },
      ] } },
    })
    expect(announcedCalls(message)).toEqual([
      { kind: 'tool-call', callId: 'c1', tool: 'read', input: PENDING_ARGUMENTS },
      { kind: 'tool-call', callId: 'c2', tool: 'read', input: PENDING_ARGUMENTS },
    ])
    expect(project(message, bare())).toEqual([{ kind: 'assistant', text: 'Reading both' }])
    expect(announcedCalls(event({ type: 'turn/end', data: { turn: 1, reason: { kind: 'completed' } } }))).toEqual([])
  })

  it('ignores a compaction rewrite of a tool result', () => {
    const data = { message: { content: [{ toolCallId: 'c1', isError: false, content: 'ok' }] } }
    expect(project(event({ type: 'tool/result', surfaceOp: 'append', data }), bare()))
      .toEqual([{ kind: 'tool-result', callId: 'c1', ok: true, text: 'ok' }])
    expect(project(event({ type: 'tool/result', surfaceOp: 'replace', data }), bare())).toEqual([])
  })

  it('surfaces a failed turn as an error notice, with the way to a key when one is missing', () => {
    const failed = (code: string) => project(event({
      type: 'turn/end',
      data: { turn: 1, reason: { kind: 'error', error: { code, message: 'no API key' } } },
    }), bare())
    expect(failed('SERVER')).toEqual([{ kind: 'notice', placement: 'turn-end', tone: 'error', text: 'SERVER: no API key' }])
    expect(failed('MISSING_CREDENTIAL')).toEqual([{ kind: 'notice', placement: 'turn-end', tone: 'error',
      text: `MISSING_CREDENTIAL: no API key\n${dictionaries.en.missingCredentialHint}` }])
    expect(failed('AUTH')[0]).toMatchObject({ text: `AUTH: no API key\n${dictionaries.en.authFailedHint}` })
  })

  it.each(['en'] as const)('gives a hook halt its recorded reason (%s)', locale => {
    const copy = dictionaries[locale]
    const ended = (reason: object) => project(event({ type: 'turn/end', data: { turn: 3, reason: { kind: 'aborted', reason } } }), projector(copy, () => undefined))
    expect(ended({ kind: 'hook', reason: 'budget exhausted' }))
      .toEqual([{ kind: 'notice', placement: 'turn-end', tone: 'warn', text: `${copy.turnHalted}: budget exhausted` }])
    expect(ended({ kind: 'user' }))
      .toEqual([{ kind: 'notice', placement: 'turn-end', tone: 'warn', text: copy.cancelled }])
  })

  it('words a cancelled turn and a compaction in the terminal copy', () => {
    expect(project(event({ type: 'turn/end', data: { turn: 1, reason: { kind: 'interrupted' } } }), bare()))
      .toEqual([{ kind: 'notice', placement: 'turn-end', tone: 'warn', text: dictionaries.en.cancelled }])
    expect(project(event({ type: 'compaction/summary', data: {} }), projector(dictionaries.en, () => undefined)))
      .toEqual([{ kind: 'notice', tone: 'info', text: dictionaries.en.compacted, compaction: true }])
  })

  it.each(['en'] as const)('renders each turn ending without treating it as agent idle (%s)', locale => {
    const copy = dictionaries[locale]
    const endings = [
      ['completed', copy.turnCompleted, 'info'],
      ['blocked', copy.turnBlocked, 'warn'],
      ['max-tokens', copy.turnMaxTokens, 'warn'],
      ['aborted', copy.cancelled, 'warn'],
      ['plugin-stop', 'plugin-stop', 'warn'],
    ] as const
    for (const [kind, label, tone] of endings) {
      expect(project(event({ type: 'turn/end', data: { turn: 7, reason: { kind } } }), projector(copy, () => undefined)))
        .toEqual([{ kind: 'notice', placement: 'turn-end', tone, text: label }])
    }
    expect(project(event({ type: 'turn/end', surfaceOp: 'replace', data: { turn: 7, reason: { kind: 'completed' } } }), bare()))
      .toEqual([])
  })

  it('keeps a command apart from the words the user sent the model', () => {
    expect(project(event({ type: 'command/run', data: { name: 'model', args: ' deepseek/chat' } }), bare()))
      .toEqual([{ kind: 'command', name: 'model', args: ' deepseek/chat' }])
    expect(project(event({ type: 'command/run', data: { name: 'model' } }), bare()))
      .toEqual([{ kind: 'command', name: 'model', args: '', inputOmitted: true }])
  })

  it('presents a call through the tool that declared how it reads', () => {
    const seam = projector(dictionaries.en, name => name === 'bash'
      ? { presentCall: (args: unknown) => ({ card: 'terminal' as const, title: (args as { command: string }).command, description: 'List the directory' }) }
      : undefined)
    expect(project(event({
      type: 'tool/call',
      data: { callId: 'c1', name: 'bash', arguments: '{"command":"ls -a"}' },
    }), seam)).toEqual([{
      kind: 'tool-call', callId: 'c1', tool: 'bash', input: 'ls -a',
      detail: [{ text: 'List the directory' }],
    }])
  })

  it('reads the arguments as fields when no tool claims the call', () => {
    expect(project(event({
      type: 'tool/call',
      data: { callId: 'c1', name: 'mystery', arguments: '{"a":1}' },
    }), bare())).toEqual([{ kind: 'tool-call', callId: 'c1', tool: 'mystery', input: 'a: 1' }])
  })

  it('heads a call by the field it acts on, and keeps text that is not a JSON object as sent', () => {
    expect(argumentsTitle('{"command":"cargo check 2>&1 | head","description":"Check"}')).toBe('cargo check 2>&1 | head')
    expect(argumentsTitle('{"file_path":"/x.md","limit":5}')).toBe('/x.md')
    expect(argumentsTitle('{"a":"b","n":[1,2]}')).toBe('a: b, n: [1,2]')
    for (const raw of ['{"pa', '[1,2]', '"text"']) expect(argumentsTitle(raw)).toBe(raw)
  })

  it('summarizes a nested list by its first item and a count, never as a JSON dump', () => {
    const questions = Array.from({ length: 3 }, (_, index) => ({
      id: `q${index}`, header: 'Choose scope', question: 'Which parts should I include? '.repeat(10),
      options: [{ label: 'Tooling swaps', description: 'Replace the remaining invocations. '.repeat(8) }],
    }))
    const title = argumentsTitle(JSON.stringify({ questions }))
    expect(title).toBe('questions: [q0, +2]')
    expect(title).not.toContain('{')
    // A list of plain values that fits stays JSON; one that does not is summarized.
    expect(argumentsTitle(JSON.stringify({ tags: ['a', 'b'], words: Array.from({ length: 30 }, () => 'word') })))
      .toBe('tags: ["a","b"], words: [word, +29]')
    expect(argumentsTitle(JSON.stringify({ empty: '', none: null, list: [], record: {}, flags: { dry: true, n: 2 } })))
      .toBe('empty: "", none: null, list: [], record: {}, flags: {dry: true, n: 2}')
    expect(argumentsTitle(JSON.stringify({ meta: { name: 'build', notes: 'x'.repeat(80) } }))).toMatch(/^meta: \{name: build, …\}$/u)
  })

  it('cuts a long value at its field bound and the headline at its own, with an ellipsis', () => {
    const title = argumentsTitle(JSON.stringify({ prompt: 'Read the module.\n\nThen explain '.repeat(20), description: 'Short' }))
    // Each field keeps room for the next, on one line.
    expect(title).toMatch(/^prompt: Read the module\. Then explain .*…, description: Short$/u)
    expect(title).not.toContain('\n')
    const fields = Object.fromEntries(Array.from({ length: 20 }, (_, index) => [`field${index}`, 'value '.repeat(10)]))
    const many = argumentsTitle(JSON.stringify(fields))
    expect(stringWidth(many)).toBe(HEADLINE_CELLS)
    expect(many.endsWith('\u2026')).toBe(true)
  })

  it('bounds a primary field and malformed arguments on their first line only', () => {
    const query = 'x'.repeat(300)
    expect(argumentsTitle(JSON.stringify({ query }))).toBe(`${'x'.repeat(HEADLINE_CELLS - 1)}\u2026`)
    // Further lines stay under the head, where the output bound applies.
    expect(argumentsTitle(JSON.stringify({ command: `echo ${'y'.repeat(200)}\necho two` })).split('\n'))
      .toEqual([`echo ${'y'.repeat(HEADLINE_CELLS - 6)}\u2026`, 'echo two'])
    const malformed = `{"broken": ${'"x", '.repeat(100)}`
    expect(argumentsTitle(malformed)).toBe(`${malformed.slice(0, HEADLINE_CELLS - 1).trimEnd()}\u2026`)
  })

  it('measures wide and combined characters in cells, and never splits one', () => {
    // Nineteen two-cell characters and the ellipsis fill 39 of the field's 40
    // cells; a twentieth would overrun it by one.
    const wide = argumentsTitle(JSON.stringify({ title: '\u4e2d\u6587\u6807\u9898'.repeat(40) }))
    expect(wide).toBe(`title: ${'\u4e2d\u6587\u6807\u9898'.repeat(4)}\u4e2d\u6587\u6807\u2026`)
    expect(stringWidth(wide)).toBe('title: '.length + 39)
    const family = '\u{1f469}\u200d\u{1f469}\u200d\u{1f467}'
    const joined = argumentsTitle(JSON.stringify({ query: family.repeat(80) }))
    expect(stringWidth(joined)).toBeLessThanOrEqual(HEADLINE_CELLS)
    expect(joined.slice(0, -1).split(family).every(part => part === '')).toBe(true)
  })

  it('strips escape sequences and shows control characters in a headline', () => {
    expect(argumentsTitle(JSON.stringify({ note: '\u001b[31mred\u001b[0m\tand\nnext\u0007' }))).toBe('note: red and next\\x07')
    expect(argumentsTitle(JSON.stringify({ path: '\u001b[2Ja.ts' }))).toBe('a.ts')
  })

  it('renders a result card in place of the model-facing text', () => {
    const seam = projector(dictionaries.en, () => ({
      presentCall: () => ({ card: 'generic' as const, title: 'Read notes.md' }),
      presentResult: () => ({ card: 'read' as const, path: 'notes.md', offset: 1, totalLines: 2,
        lines: [{ number: 1, text: 'first' }, { number: 2, text: 'second' }] }),
    }))
    project(event({ type: 'tool/call', data: { callId: 'c1', name: 'read', arguments: '{}' } }), seam)
    expect(project(event({
      type: 'tool/result',
      data: { message: { content: [{ toolCallId: 'c1', isError: false, content: 'raw envelope text' }] } },
    }), seam)).toEqual([{
      kind: 'tool-result', callId: 'c1', ok: true, text: '',
      detail: [{ text: '1  first', source: 'notes.md', codeOffset: 3, number: 1, codeStart: true },
        { text: '2  second', source: 'notes.md', codeOffset: 3, number: 2 }],
    }])
  })

  it('marks failed work in a successful tool result as failed in the UI', () => {
    const seam = projector(dictionaries.en, name => name === 'bash' ? {
      presentCall: () => ({ card: 'terminal' as const, title: 'false' }),
      presentResult: () => ({ card: 'terminal' as const, output: '', exitCode: 2 }),
    } : undefined)
    project(event({ type: 'tool/call', data: { callId: 'b1', name: 'bash', arguments: '{}' } }), seam)
    expect(project(event({
      type: 'tool/result',
      data: { message: { content: [{ toolCallId: 'b1', isError: false, content: '[exit code: 2]' }] } },
    }), seam)).toEqual([{
      kind: 'tool-result', callId: 'b1', ok: false, text: '',
      detail: [{ text: 'exit 2', summary: 'failure' }],
    }])
  })

  it('keeps the model-facing text under a generic result that omits its content, cut as a raw result is', () => {
    // A plugin tool's presenters: a call titled by the run's name, and a
    // result card that reformats nothing.
    const seam = projector(dictionaries.en, name => name === 'orchestrate' ? {
      presentCall: (args: unknown) => ({ card: 'generic' as const, title: `orchestrate: ${(args as { meta: { name: string } }).meta.name}` }),
      presentResult: () => ({ card: 'generic' as const }),
    } : undefined)
    project(event({ type: 'tool/call', data: { callId: 'w1', name: 'orchestrate', arguments: '{"script":"return 1","meta":{"name":"audit"}}' } }), seam)
    const summary = 'finding '.repeat(40).trim()
    const text = `run "audit" completed (2 agents).\nReturn value:\n${JSON.stringify({ summary }, null, 2)}`
    const rows = project(event({
      type: 'tool/result',
      data: { message: { content: [{ toolCallId: 'w1', isError: false, content: text }] } },
    }), seam)
    expect(rows).toEqual([{ kind: 'tool-result', callId: 'w1', ok: true, text }])
    const long = `  "summary": "${summary}"`
    expect(present(rows[0]!, { lines: 8, unit: 'lines', more: 'more lines' }).map(line => line.text)).toEqual([
      '[w1]  5 lines', 'run "audit" completed (2 agents).', 'Return value:', '{',
      `${long.slice(0, RAW_LINE_CELLS - 1).trimEnd()}\u2026`, '}',
    ])
  })

  it('survives a tool whose presenter throws', () => {
    const seam = projector(dictionaries.en, () => ({ presentCall: () => { throw new Error('bad args') } }))
    expect(project(event({
      type: 'tool/call',
      data: { callId: 'c1', name: 'brittle', arguments: '{"a":1}' },
    }), seam)).toEqual([{ kind: 'tool-call', callId: 'c1', tool: 'brittle', input: 'a: 1' }])
  })

  it('renders nothing for an event type this build does not know', () => {
    expect(project(event({ type: 'some/future/event', data: {} }), bare())).toEqual([])
  })
})

describe('outputRate', () => {
  /** A committed answer whose first token arrived at 1000 and which finished at `finish`. */
  const answer = (options: { finish?: string, outputTokens?: number, interrupted?: true, at?: number } = {}) => ({
    turn: 1, step: 1,
    message: { role: 'assistant', content: [{ type: 'text', text: 'Done.' }] },
    ...options.outputTokens === 0 ? {} : { usage: { inputTokens: 10, outputTokens: options.outputTokens ?? 120 } },
    ...options.interrupted === undefined ? {} : { interrupted: true },
    stream: [
      { type: 'chunk', time: 400, chunk: { type: 'block-start', index: 0, blockType: 'reasoning' } },
      { type: 'reasoning-chunks', time0: 1000, index: 0, dt: [50], texts: ['Hm', 'm'] },
      { type: 'text-chunks', time0: 1500, index: 1, dt: [], texts: ['Done.'] },
      { type: 'chunk', time: options.at ?? 4000, chunk: { type: 'finish', reason: { kind: options.finish ?? 'stop' } } },
    ],
  }) as never

  it('divides the reported output tokens by the time from the first token to the finish', () => {
    // The wait before the first token, from 400, is not generation time.
    expect(outputRate(answer())).toEqual({ tokens: 120, ms: 3000 })
  })

  it('reports nothing for a step that is not a whole final answer', () => {
    expect(outputRate(answer({ finish: 'tool-calls' }))).toBeUndefined()
    expect(outputRate(answer({ interrupted: true }))).toBeUndefined()
    expect(outputRate(answer({ outputTokens: 0 }))).toBeUndefined()
    expect(outputRate(answer({ at: 1000 }))).toBeUndefined()
  })

  it('keeps the rate after the answer it measures, and only after an answer, and draws nothing for it', () => {
    const rows = project(event({ type: 'assistant/message', data: answer() }), bare())
    expect(rows).toEqual([{ kind: 'assistant', text: 'Done.' }, { kind: 'rate', tokens: 120, ms: 3000 }])
    // The turn's summary reports it; a row of its own under the answer split the turn's numbers.
    expect(present(rows[1]!, { lines: 4, unit: 'lines', more: 'more lines' })).toEqual([])
    const silent = { ...(answer() as object), message: { role: 'assistant', content: [{ type: 'reasoning', text: 'Hmm' }] } }
    expect(project(event({ type: 'assistant/message', data: silent }), bare())).toEqual([{ kind: 'reasoning', text: 'Hmm' }])
  })
})
