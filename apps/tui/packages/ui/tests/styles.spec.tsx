/** ANSI emphasis for reasoning, actions, and their results. */
import { execFileSync } from 'node:child_process'
import { expect, it } from 'vitest'
import type { Row } from '../src/rows.ts'
import { AGENT_TONES, CONTEXT_RAMP, PALETTE } from '../src/palette.ts'
import { ICON } from '../src/icons.ts'
import { subagentSheet } from '../src/subagents.tsx'
import { dictionaries } from '../src/copy.ts'
import { FOLD_REST } from '../src/activity.ts'

it('draws the rule dim and leaves the draft in the terminal\'s own foreground at every width', () => {
  const env: NodeJS.ProcessEnv = { ...process.env, FORCE_COLOR: '3', COLORTERM: 'truecolor' }
  delete env.NO_COLOR
  const widths = [12, 40, 80, 200]
  const output = execFileSync(process.execPath, ['--import', 'tsx/esm', '--input-type=module', '--eval', `
    import React from 'react';
    import { renderToString } from 'ink';
    import { Chrome } from ${JSON.stringify(new URL('../src/line.tsx', import.meta.url).href)};
    const hints = { send: 'Enter sends', interrupt: 'Esc stops', select: 'Tab selects', answer: 'Enter sends' };
    for (const columns of ${JSON.stringify(widths)}) process.stdout.write(renderToString(React.createElement(Chrome, {
      status: [{ forms: [[{ text: 'm' }]], yields: [] }], columns, state: { running: false, asking: false, listing: false },
      before: 'hello', after: '', placeholder: 'Ask', hints, frame: 'round',
    }), { columns }) + '\\nEND\\n');
  `], { cwd: new URL('../../../../../', import.meta.url), env, encoding: 'utf8', timeout: 20_000 })
  const rendered = output.split('END\n').filter(Boolean)
  expect(rendered).toHaveLength(widths.length)
  for (const [index, frame] of rendered.entries()) {
    const columns = widths[index]!
    const rows = frame.trimEnd().split('\n')
    // Gap, header, rule, draft, base rule, status.
    expect(rows, `${columns}`).toHaveLength(6)
    expect(rows[2]).toBe(`\u001b[2m${'\u2500'.repeat(columns)}\u001b[22m`)
    // No background anywhere, and the draft carries no colour of its own, so
    // it reads on a light theme as well as a dark one.
    expect(frame).not.toMatch(/\u001b\[48[;:]/)
    // The caret is a reverse-video cell after the text, in no colour of its own.
    expect(rows[3]).toContain(' hello\u001b[7m \u001b[27m')
    expect(rows[3]).not.toMatch(/\u001b\[38[;:][0-9;:]*mhello/)
    expect(rows[4]).toBe(`\u001b[2m${'\u2500'.repeat(columns)}\u001b[22m`)
  }
})

it('dims reasoning and metadata, and gives actions and outcomes their palette weight and colour', async () => {
  const rows: Row[] = [
    { kind: 'reasoning', text: 'Check the files.\nThen verify the change.' },
    { kind: 'tool-call', callId: 'c1', tool: 'bash', input: 'ls -a', detail: [{ text: 'List the directory' }],
      result: { ok: true, text: 'one.ts\ntwo.ts\nthree.ts\nfour.ts' } },
    { kind: 'tool-call', callId: 'c2', tool: 'str_replace_editor', input: 'one.ts', result: { ok: true, text: '', title: 'Edit one.ts', detail: [
      { text: '- const a = old', emphasis: 'removed', source: 'one.ts', number: 4, changed: [[12, 15]] },
      { text: '+ const a = new', emphasis: 'added', source: 'one.ts', number: 4, changed: [[12, 15]] },
    ] } },
    { kind: 'tool-call', callId: 'c3', tool: 'read_file', input: 'Read secret.md', result: { ok: false, text: 'Permission denied' } },
    { kind: 'tool-call', callId: 'c4', tool: 'grep', input: 'TODO' },
    { kind: 'assistant', text: 'Updated one.ts.' },
  ]
  // Chalk reads color support during module initialization; a child gives this
  // render its own environment without changing other Ink tests' styling.
  // Truecolour is forced so the assertion pins the palette's exact tones rather
  // than whichever colour the host terminal rounds them to. `FORCE_COLOR` only
  // sets a floor that a `TERM` of `xterm-256color` still overrides to 256
  // colours. `COLORTERM` is what chalk treats as truecolour there.
  const env: NodeJS.ProcessEnv = { ...process.env, FORCE_COLOR: '3', COLORTERM: 'truecolor' }
  delete env.NO_COLOR
  const frame = execFileSync(process.execPath, ['--import', 'tsx/esm', '--input-type=module', '--eval', `
    import React from 'react';
    import { renderToString } from 'ink';
    import { Line } from ${JSON.stringify(new URL('../src/line.tsx', import.meta.url).href)};
    import { present } from ${JSON.stringify(new URL('../src/present.ts', import.meta.url).href)};
    import { budgetFor } from ${JSON.stringify(new URL('../src/layout.ts', import.meta.url).href)};
    const budget = budgetFor({ columns: 80, rows: 40 });
    const preview = { lines: 3, unit: 'lines', more: 'more lines' };
    const lines = ${JSON.stringify(rows)}.flatMap(row => present(row, preview));
    process.stdout.write(renderToString(React.createElement(React.Fragment, null,
      ...lines.map((line, key) => React.createElement(Line, { line, key, budget, frame: 'classic' }))), { columns: 80 }));
  `], { cwd: new URL('../../../../../', import.meta.url), env, encoding: 'utf8', timeout: 20_000 })
  // Reasoning is a dim, italic paragraph at the rail with no verb, so it is
  // the working-out, not output and not the answer.
  expect(frame).not.toContain('think')
  expect(frame).toContain('  \u001b[3m\u001b[2mCheck the files.')
  // The tool's name opens an action in bold, its argument in parentheses
  // beside it; markers carry the outcome colours, and a dim connector hangs
  // the output from the head.
  expect(frame).toContain('\u001b[1mBash\u001b[22m(ls -a)')
  expect(frame).toContain('\u001b[2m\u23bf\u001b[22m')
  expect(frame).toContain(`\u001b[1m\u001b[38;2;34;197;94m${ICON.other}`)
  expect(frame).toContain(`\u001b[38;2;239;68;68m${ICON.other}`)
  expect(frame).toContain(`\u001b[1m\u001b[38;2;249;115;22m${ICON.other}`)
  expect(frame).not.toContain('\u001b[2mBash')
  // An edit says its size in each side's tone, numbers its lines in the
  // gutter, and reverses the words it changed.
  expect(frame).toContain('\u001b[38;2;34;197;94m  +1\u001b[39m \u001b[38;2;239;68;68m−1')
  // The number stays in the verb column, in its side's tone, and the code
  // follows it after one cell; a preview draws no background anywhere.
  const output = `\u001b[38;2;${[1, 3, 5].map(index => Number.parseInt(PALETTE.output.slice(index, index + 2), 16)).join(';')}m`
  expect(frame).not.toMatch(/\u001b\[48[;:]/)
  expect(frame).toMatch(/\u001b\[38;2;239;68;68m     4 (?:\u001b\[[0-9;]*m)*- const a = \u001b\[7mold/)
  expect(frame).toMatch(/\u001b\[38;2;34;197;94m     4 (?:\u001b\[[0-9;]*m)*\+ const a = \u001b\[7mnew/)
  // Output keeps its syntax colour; the call's own description stays dim.
  expect(frame).toContain('\u001b[38;2;96;165;250mone.ts')
  expect(frame).toMatch(/\u001b\[2mList the directory\u001b\[22m\n/)
  // A failure's text stays red in the preview, never the output grey.
  expect(frame).toContain('\u001b[38;2;239;68;68mPermission denied')
  expect(frame).not.toContain(`${output}Permission denied`)
  await expect(frame.replaceAll('\u001b', '<ESC>') + '\n').toMatchFileSnapshot('./expected/styles.txt')
})

it('draws a step\'s tree dim and uncoloured, badges each call with its state, and puts a failed call\'s failure in its head', () => {
  const rows: Row[] = [{ kind: 'tool-group', calls: [
    { kind: 'tool-call', callId: 'a', tool: 'bash', input: 'make', result: { ok: true, text: 'one\ntwo' } },
    { kind: 'tool-call', callId: 'b', tool: 'bash', input: 'false', result: { ok: false, text: 'boom' } },
  ] }]
  const env: NodeJS.ProcessEnv = { ...process.env, FORCE_COLOR: '3', COLORTERM: 'truecolor' }
  delete env.NO_COLOR
  const frame = execFileSync(process.execPath, ['--import', 'tsx/esm', '--input-type=module', '--eval', `
    import React from 'react';
    import { renderToString } from 'ink';
    import { Line } from ${JSON.stringify(new URL('../src/line.tsx', import.meta.url).href)};
    import { present } from ${JSON.stringify(new URL('../src/present.ts', import.meta.url).href)};
    import { budgetFor } from ${JSON.stringify(new URL('../src/layout.ts', import.meta.url).href)};
    const lines = ${JSON.stringify(rows)}.flatMap(row => present(row, { lines: 3, unit: 'lines', more: 'more lines', failures: 'failed' }));
    process.stdout.write(renderToString(React.createElement(React.Fragment, null,
      ...lines.map((line, key) => React.createElement(Line, { line, key, budget: budgetFor({ columns: 80, rows: 40 }), frame: 'classic' }))), { columns: 80 }));
  `], { cwd: new URL('../../../../../', import.meta.url), env, encoding: 'utf8', timeout: 20_000 })
  const rgb = (hex: string) => `\u001b[38;2;${[1, 3, 5].map(index => Number.parseInt(hex.slice(index, index + 2), 16)).join(';')}m`
  // The step's head keeps its state colour.
  expect(frame).toContain(`${rgb(PALETTE.failed)}${ICON.other}`)
  // Every glyph of the tree is dim, in the terminal's own tone: no palette colour precedes one.
  for (const glyph of ['\u251c', '\u2502', '\u2514', '\u23bf']) {
    expect(frame, glyph).toContain(`\u001b[2m${glyph}`)
    expect(frame, glyph).not.toMatch(new RegExp(`\\u001b\\[38;2;[0-9;]*m(?:\\u001b\\[[0-9;]*m)*${glyph}`))
  }
  // Past the quiet branch, each call keeps its own marker as a badge in its outcome's colour.
  // The failed call reads as failed in its head too: its name bold and red, its argument red.
  expect(frame).toContain(`\u001b[2m\u2514\u001b[22m ${rgb(PALETTE.failed)}${ICON.other}\u001b[39m \u001b[1m${rgb(PALETTE.failed)}Bash\u001b[22m(false)\u001b[39m`)
  // The call that finished well has a green badge, and its head stays plain.
  expect(frame).toContain(`\u001b[2m\u251c\u001b[22m \u001b[1m${rgb(PALETTE.done)}${ICON.other}\u001b[39m\u001b[22m \u001b[1mBash\u001b[22m(make)`)
})

it('colours the running header\'s word, leaves the rule bare, and keeps the status line neutral until a reading needs attention', () => {
  // A child for its own colour environment, as above.
  const env: NodeJS.ProcessEnv = { ...process.env, FORCE_COLOR: '3', COLORTERM: 'truecolor' }
  delete env.NO_COLOR
  const frame = execFileSync(process.execPath, ['--import', 'tsx/esm', '--input-type=module', '--eval', `
    import React from 'react';
    import { renderToString } from 'ink';
    import { Header, Rule, StatusBar } from ${JSON.stringify(new URL('../src/line.tsx', import.meta.url).href)};
    import { Beat } from ${JSON.stringify(new URL('../src/beat.tsx', import.meta.url).href)};
    import { FRAME_MS } from ${JSON.stringify(new URL('../src/activity.ts', import.meta.url).href)};
    import { PALETTE } from ${JSON.stringify(new URL('../src/palette.ts', import.meta.url).href)};
    import { statusFields } from ${JSON.stringify(new URL('../src/status-line.ts', import.meta.url).href)};
    import { dictionaries } from ${JSON.stringify(new URL('../src/copy.ts', import.meta.url).href)};
    // Four beats in.
    const clock = { now: () => 4 * FRAME_MS, every: () => () => {} };
    const row = (input) => React.createElement(StatusBar, { key: JSON.stringify(input), columns: 120,
      fields: statusFields({ model: 'm', cwd: '/w', glyphs: 'unicode', ...input }, dictionaries.en) });
    process.stdout.write(renderToString(React.createElement(React.Fragment, null,
      React.createElement(Beat, { clock },
        React.createElement(Header, { columns: 100, clock,
          state: { kind: 'running', word: 'Working', phase: undefined, startedAt: 0, color: PALETTE.running } })),
      React.createElement(Rule, { columns: 100, frame: 'round' }),
      ...[850, 480, 120].map(cached => row({ thinkingLevel: 'medium',
        context: { used: 500, window: 128_000 }, usage: { input: 1000, output: 100, cached } })),
      ...[65_000, 75_000, 85_000, 95_000].map(used => row({ context: { used, window: 100_000 } })),
      row({ context: { used: 62_000, window: 100_000, compactAt: 80_000 } }),
      ...['low', 'high', 'xhigh', 'max'].map(thinkingLevel => row({ thinkingLevel }))),
      { columns: 120 }));
  `], { cwd: new URL('../../../../../', import.meta.url), env, encoding: 'utf8', timeout: 20_000 })
  const rgb = (hex: string) => `\u001b[38;2;${[1, 3, 5].map(index => Number.parseInt(hex.slice(index, index + 2), 16)).join(';')}m`
  // The header's glyph and word carry the running orange, at the draft's
  // column; the rule under it is one dim run and says nothing.
  const [header, rule, ...rows] = frame.split('\n')
  expect(header!.replace(/\u001b\[[0-9;]*m/g, '').trimEnd()).toMatch(/^[\u2800-\u28ff]{3} Working… {2}0s$/)
  expect(header).toContain(`${rgb(PALETTE.running)}Working…`)
  expect(rule).toBe(`\u001b[2m${'─'.repeat(100)}\u001b[22m`)
  // The model has no label and the normal foreground; labels are dim, values are not.
  expect(rows[0]!.startsWith('m ')).toBe(true)
  expect(rows[0]).toContain('\u001b[2mthink \u001b[22mmedium')
  expect(rows[0]).toContain('\u001b[2mctx \u001b[22m~0% (500/128k)')
  expect(rows[0]).toContain('\u001b[2m/w\u001b[22m')
  // The totals and the cache hit's label are one dim run. A healthy hit is
  // plain; a middling one yellow and a low one red.
  expect(rows[0]).toContain('\u001b[2min 1k  out 100  cache hit \u001b[22m85%')
  expect(rows[1]).toContain(`cache hit \u001b[22m${rgb(PALETTE.waiting)}48%`)
  expect(rows[2]).toContain(`${rgb(PALETTE.failed)}12%`)
  // Nothing on a healthy row is coloured at all.
  expect(rows[0]).not.toContain('\u001b[38;')
  // The context reading warms through the ramp as it fills, the label left dim.
  for (const [index, reading] of ['~65%', '~75%', '~85%', '~95%'].entries()) {
    expect(rows[3 + index]).toContain(`\u001b[2mctx \u001b[22m${rgb(CONTEXT_RAMP[index]!)}${reading}`)
  }
  // The compaction mark is dim beside a reading that turned yellow ten points under orange.
  expect(rows[7]).toContain(`${rgb(CONTEXT_RAMP[1])}~62% (62k/100k)\u001b[39m\u001b[2m · compacts at 80%\u001b[22m`)
  // The thinking level warms with the effort: a light one dim with its label, then blue, orange, and pink.
  expect(rows[8]).toContain('\u001b[2mthink low\u001b[22m')
  for (const [index, [level, tone]] of ([['high', PALETTE.asking], ['xhigh', CONTEXT_RAMP[2]], ['max', AGENT_TONES[1]]] as const).entries()) {
    expect(rows[9 + index]).toContain(`\u001b[2mthink \u001b[22m${rgb(tone)}${level}`)
  }
})

it('colours compaction blue, by /compact or inside a turn, never the turn\'s orange, and the notice it leaves the same', () => {
  // A child for its own colour environment, as above.
  const env: NodeJS.ProcessEnv = { ...process.env, FORCE_COLOR: '3', COLORTERM: 'truecolor' }
  delete env.NO_COLOR
  const frames = execFileSync(process.execPath, ['--import', 'tsx/esm', '--input-type=module', '--eval', `
    import React from 'react';
    import { renderToString } from 'ink';
    import { App } from ${JSON.stringify(new URL('../src/app.tsx', import.meta.url).href)};
    import { Line } from ${JSON.stringify(new URL('../src/line.tsx', import.meta.url).href)};
    import { present } from ${JSON.stringify(new URL('../src/present.ts', import.meta.url).href)};
    import { budgetFor } from ${JSON.stringify(new URL('../src/layout.ts', import.meta.url).href)};
    import { dictionaries } from ${JSON.stringify(new URL('../src/copy.ts', import.meta.url).href)};
    import { emptyTranscript } from ${JSON.stringify(new URL('../src/transcript.ts', import.meta.url).href)};
    const noop = () => {};
    const props = {
      files: { query: undefined, entries: [], loading: false, error: undefined }, onReferenceQuery: noop,
      completion: { entries: [], loading: false, error: undefined }, completionLimit: 8, resultLines: 8,
      committed: emptyTranscript, live: [], pending: [], status: 'idle', stopping: false, command: undefined,
      notice: undefined, interaction: undefined, model: 'mock/model', cwd: '/w', sessionId: 's',
      copy: dictionaries.en, frame: 'round', quitting: false, context: undefined,
      onSubmit: noop, onCancel: noop, onInterrupt: noop, onAnswer: noop,
    };
    for (const state of [{ command: '/compact', compactPhase: 'summarizing' }, { status: 'running', autoCompacting: true }]) {
      process.stdout.write(renderToString(React.createElement(App, { ...props, ...state }), { columns: 80 }) + '\\nEND\\n');
    }
    // The transcript's mark where history was compacted.
    const notice = present({ kind: 'notice', tone: 'info', text: dictionaries.en.compacted, compaction: true }, { lines: 3, unit: 'lines', more: 'more' });
    process.stdout.write(renderToString(React.createElement(React.Fragment, null, ...notice.map((line, key) =>
      React.createElement(Line, { line, key, budget: budgetFor({ columns: 80, rows: 40 }), frame: 'classic' }))), { columns: 80 }) + '\\nEND\\n');
  `], { cwd: new URL('../../../../../', import.meta.url), env, encoding: 'utf8', timeout: 20_000 })
  const rgb = (hex: string) => `\u001b[38;2;${[1, 3, 5].map(index => Number.parseInt(hex.slice(index, index + 2), 16)).join(';')}m`
  const rendered = frames.split('END\n').filter(frame => frame.trim() !== '')
  expect(rendered).toHaveLength(3)
  for (const frame of rendered.slice(0, 2)) {
    const header = frame.split('\n').find(line => line.includes(dictionaries.en.compacting))!
    expect(header.replace(/\u001b\[[0-9;]*m/g, '')).toBe(`${FOLD_REST} ${dictionaries.en.compacting}…  ${dictionaries.en.compactSummarizing}`)
    expect(header).toContain(`${rgb(PALETTE.compacting)}${FOLD_REST}`)
    expect(header).toContain(`${rgb(PALETTE.compacting)}${dictionaries.en.compacting}…`)
    expect(header).not.toContain(rgb(PALETTE.running))
  }
  // The notice keeps the same blue, neither dim nor bold.
  expect(rendered[2]).toContain(`${rgb(PALETTE.compacting)}${dictionaries.en.compacted}`)
  expect(rendered[2]).not.toContain(`\u001b[2m${dictionaries.en.compacted}`)
})

it('colours the permission boundary beside a dim label without relying on colour for its name', () => {
  const env: NodeJS.ProcessEnv = { ...process.env, FORCE_COLOR: '3', COLORTERM: 'truecolor' }
  delete env.NO_COLOR
  const frame = execFileSync(process.execPath, ['--import', 'tsx/esm', '--input-type=module', '--eval', `
    import React from 'react';
    import { renderToString } from 'ink';
    import { Welcome } from ${JSON.stringify(new URL('../src/welcome.tsx', import.meta.url).href)};
    import { dictionaries } from ${JSON.stringify(new URL('../src/copy.ts', import.meta.url).href)};
    const modes = ['read-only', 'workspace-write', 'danger-full-access', 'custom', 'auto'];
    process.stdout.write(renderToString(React.createElement(React.Fragment, null,
      ...modes.map(access => React.createElement(Welcome, { key: access, version: '1.0.0', heading: 'Session: s', access,
        copy: dictionaries.en, frame: 'round', columns: 80 }))), { columns: 80 }));
  `], { cwd: new URL('../../../../../', import.meta.url), env, encoding: 'utf8', timeout: 20_000 })
  for (const [mode, tone] of [['read-only', PALETTE.reference], ['workspace-write', PALETTE.done],
    ['danger-full-access', PALETTE.failed], ['custom', PALETTE.waiting], ['auto', PALETTE.waiting]] as const) {
    const rgb = [1, 3, 5].map(index => Number.parseInt(tone.slice(index, index + 2), 16)).join(';')
    expect(frame).toContain(`\u001b[2mAccess \u001b[22m\u001b[38;2;${rgb}m${mode}`)
  }
})

it('dims the subagents row but its working rail, and draws each child in its own tone on the sheet', () => {
  const env: NodeJS.ProcessEnv = { ...process.env, FORCE_COLOR: '3', COLORTERM: 'truecolor' }
  delete env.NO_COLOR
  const frame = execFileSync(process.execPath, ['--import', 'tsx/esm', '--input-type=module', '--eval', `
    import React from 'react';
    import { renderToString } from 'ink';
    import { SubagentRow } from ${JSON.stringify(new URL('../src/subagents.tsx', import.meta.url).href)};
    import { dictionaries } from ${JSON.stringify(new URL('../src/copy.ts', import.meta.url).href)};
    const entries = ['Review', 'Check', 'Audit'].map((label, index) =>
      ({ id: label, label, state: index === 0 ? 'working' : 'saved', detail: '', inspectable: true }));
    process.stdout.write(renderToString(React.createElement(SubagentRow, { entries, copy: dictionaries.en, columns: 100 }), { columns: 100 }));
  `], { cwd: new URL('../../../../../', import.meta.url), env, encoding: 'utf8', timeout: 20_000 })
  const rgb = (hex: string) => `\u001b[38;2;${[1, 3, 5].map(index => Number.parseInt(hex.slice(index, index + 2), 16)).join(';')}m`
  // The rail takes the running colour while a child works; the counts are dim,
  // with no identity tone, since names are the sheet's.
  expect(frame.startsWith(`\u001b[1m${rgb(PALETTE.running)}\u21b3`)).toBe(true)
  expect(frame).toContain('\u001b[2mSubagents 3 \u00b7 1 working')
  for (const tone of AGENT_TONES) expect(frame).not.toContain(rgb(tone))
  const entries = ['Review', 'Check', 'Audit'].map((label, index) =>
    ({ id: label, label, state: index === 0 ? 'working' as const : 'saved' as const, detail: '', inspectable: true }))
  const names = subagentSheet(entries, 0, dictionaries.en).filter(line => line.selected !== undefined).flatMap(line => line.parts?.slice(0, 1) ?? [])
  expect(names.map(part => [part.text, part.color])).toEqual(entries.map((entry, index) => [entry.label, AGENT_TONES[index]]))
  expect(new Set(AGENT_TONES).size).toBe(AGENT_TONES.length)
  for (const tone of AGENT_TONES) expect(Object.values(PALETTE)).not.toContain(tone)
})

it('colours the git field\'s staged paths green, unstaged yellow, and conflicts red, and leaves the rest dim', () => {
  // A child for its own colour environment, as above.
  const env: NodeJS.ProcessEnv = { ...process.env, FORCE_COLOR: '3', COLORTERM: 'truecolor' }
  delete env.NO_COLOR
  const frame = execFileSync(process.execPath, ['--import', 'tsx/esm', '--input-type=module', '--eval', `
    import React from 'react';
    import { renderToString } from 'ink';
    import { StatusBar } from ${JSON.stringify(new URL('../src/line.tsx', import.meta.url).href)};
    import { gitField } from ${JSON.stringify(new URL('../src/git.ts', import.meta.url).href)};
    const git = gitField({ branch: 'main', detached: false, ahead: 1, behind: 0, staged: 2, modified: 3, untracked: 4, conflicted: 5 }, 'unicode');
    process.stdout.write(renderToString(React.createElement(StatusBar, { fields: [git], columns: 100 }), { columns: 100 }));
  `], { cwd: new URL('../../../../../', import.meta.url), env, encoding: 'utf8', timeout: 20_000 })
  const rgb = (hex: string) => `\u001b[38;2;${[1, 3, 5].map(index => Number.parseInt(hex.slice(index, index + 2), 16)).join(';')}m`
  expect(frame).toContain(`\u001b[2m\u2387 \u001b[22mmain`)
  expect(frame).toContain(`${rgb(PALETTE.failed)} !5`)
  expect(frame).toContain(`${rgb(PALETTE.done)} +2`)
  expect(frame).toContain(`${rgb(PALETTE.waiting)} ~3`)
  // Untracked paths and the distance from the upstream share one dim run.
  expect(frame).toContain('\u001b[2m ?4 \u21911\u001b[22m')
})
