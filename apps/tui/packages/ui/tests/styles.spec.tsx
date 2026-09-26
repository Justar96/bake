/** ANSI emphasis for reasoning, actions, and their results. */
import { execFileSync } from 'node:child_process'
import { expect, it } from 'vitest'
import type { Row } from '../src/rows.ts'
import { PALETTE } from '../src/palette.ts'

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
      left: ['Model: m'], right: [], columns, state: { running: false, asking: false, listing: false },
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
    expect(rows[3]).toContain(' hello▌')
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
      ...lines.map((line, key) => React.createElement(Line, { line, key, budget }))), { columns: 80 }));
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
  expect(frame).toContain('\u001b[1m\u001b[38;2;34;197;94m\u25cf')
  expect(frame).toContain('\u001b[38;2;239;68;68m\u25cf')
  expect(frame).toContain('\u001b[1m\u001b[38;2;249;115;22m\u25cf')
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

it('colours the running header\'s word, leaves the rule bare, and colours a cache-hit reading by how good it is', () => {
  // A child for its own colour environment, as above.
  const env: NodeJS.ProcessEnv = { ...process.env, FORCE_COLOR: '3', COLORTERM: 'truecolor' }
  delete env.NO_COLOR
  const frame = execFileSync(process.execPath, ['--import', 'tsx/esm', '--input-type=module', '--eval', `
    import React from 'react';
    import { renderToString } from 'ink';
    import { Header, Rule, StatusBar } from ${JSON.stringify(new URL('../src/line.tsx', import.meta.url).href)};
    import { Beat } from ${JSON.stringify(new URL('../src/beat.tsx', import.meta.url).href)};
    import { FRAME_MS } from ${JSON.stringify(new URL('../src/activity.ts', import.meta.url).href)};
    import { cacheTone, PALETTE } from ${JSON.stringify(new URL('../src/palette.ts', import.meta.url).href)};
    // Four beats in.
    const clock = { now: () => 4 * FRAME_MS, every: () => () => {} };
    const field = hit => ({ label: 'cache hit', value: hit + '%', color: cacheTone(hit) });
    process.stdout.write(renderToString(React.createElement(React.Fragment, null,
      React.createElement(Beat, { clock },
        React.createElement(Header, { columns: 100, clock,
          state: { kind: 'running', word: 'Working', phase: undefined, startedAt: 0, color: PALETTE.running } })),
      React.createElement(Rule, { columns: 100, frame: 'round' }),
      ...[85, 48, 12].map(hit => React.createElement(StatusBar, { key: hit,
        left: [{ text: 'Model: m' }, 'plan'],
        right: [{ text: 'Context: ~500/128k (0%)' }, 'in 1k', 'out 100', field(hit), '/w'], columns: 100 }))),
      { columns: 100 }));
  `], { cwd: new URL('../../../../../', import.meta.url), env, encoding: 'utf8', timeout: 20_000 })
  const rgb = (hex: string) => `\u001b[38;2;${[1, 3, 5].map(index => Number.parseInt(hex.slice(index, index + 2), 16)).join(';')}m`
  // The header's glyph and word carry the running orange, at the draft's
  // column; the rule under it is one dim run and says nothing.
  const [header, rule] = frame.split('\n')
  expect(header!.replace(/\u001b\[[0-9;]*m/g, '').trimEnd()).toMatch(/^[\u2800-\u283f]{3} Working… {2}0s$/)
  expect(header).toContain(`${rgb(PALETTE.running)}Working…`)
  expect(rule).toBe(`\u001b[2m${'─'.repeat(100)}\u001b[22m`)
  // Primary fields use plain foreground; secondary fields retain dim styling.
  expect(frame).toContain('Model: m\u001b[2m  plan\u001b[22m  Context: ~500/128k (0%)')
  expect(frame).toContain('\u001b[2min 1k\u001b[22m')
  expect(frame).toContain('\u001b[2mout 100\u001b[22m')
  expect(frame).toContain('\u001b[2m/w\u001b[22m')
  // The label stays dim; the value alone carries the tone.
  expect(frame).toContain(`\u001b[2mcache hit \u001b[22m${rgb('#22c55e')}85%`)
  expect(frame).toContain(`${rgb('#eab308')}48%`)
  expect(frame).toContain(`${rgb('#ef4444')}12%`)
})

it('colours the permission boundary beside a dim label without relying on colour for its name', () => {
  const env: NodeJS.ProcessEnv = { ...process.env, FORCE_COLOR: '3', COLORTERM: 'truecolor' }
  delete env.NO_COLOR
  const frame = execFileSync(process.execPath, ['--import', 'tsx/esm', '--input-type=module', '--eval', `
    import React from 'react';
    import { renderToString } from 'ink';
    import { StatusBar } from ${JSON.stringify(new URL('../src/line.tsx', import.meta.url).href)};
    import { permissionTone } from ${JSON.stringify(new URL('../src/palette.ts', import.meta.url).href)};
    const modes = ['read-only', 'workspace-write', 'danger-full-access', 'custom', 'auto'];
    process.stdout.write(renderToString(React.createElement(React.Fragment, null,
      ...modes.map(value => React.createElement(StatusBar, { key: value,
        left: [{ text: 'Model: m' }], badge: { label: 'Access', value, color: permissionTone(value) },
        secondaryBadge: { label: 'Think', value: 'high', color: ${JSON.stringify(PALETTE.asking)} },
        right: [], columns: 60 }))), { columns: 60 }));
  `], { cwd: new URL('../../../../../', import.meta.url), env, encoding: 'utf8', timeout: 20_000 })
  for (const [mode, tone] of [['read-only', PALETTE.reference], ['workspace-write', PALETTE.done],
    ['danger-full-access', PALETTE.failed], ['custom', PALETTE.waiting], ['auto', PALETTE.waiting]] as const) {
    const rgb = [1, 3, 5].map(index => Number.parseInt(tone.slice(index, index + 2), 16)).join(';')
    expect(frame).toContain(`Model: m  \u001b[2mAccess \u001b[22m\u001b[38;2;${rgb}m${mode}`)
  }
  expect(frame).toContain('\u001b[2mThink \u001b[22m\u001b[38;2;14;165;233mhigh')
})
