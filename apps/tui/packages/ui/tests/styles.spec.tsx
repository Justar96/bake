/** ANSI emphasis for reasoning, actions, and their results. */
import { execFileSync } from 'node:child_process'
import { expect, it } from 'vitest'
import type { Row } from '../src/rows.ts'
import { ACCENT, PALETTE } from '../src/palette.ts'

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
  const plain = (row: string): string => row.replace(/\u001b\[[0-9;]*m/g, '')
  const rendered = output.split('END\n').filter(Boolean)
  expect(rendered).toHaveLength(widths.length)
  for (const [index, frame] of rendered.entries()) {
    const columns = widths[index]!
    const rows = frame.trimEnd().split('\n')
    // Gap, rule, draft, padding, status.
    expect(rows, `${columns}`).toHaveLength(5)
    expect(rows[1]).toBe(`\u001b[2m${'\u2500'.repeat(columns)}\u001b[22m`)
    // No background anywhere, and the draft carries no colour of its own, so
    // it reads on a light theme as well as a dark one.
    expect(frame).not.toMatch(/\u001b\[48[;:]/)
    expect(rows[2]).toContain(' hello▌')
    expect(rows[2]).not.toMatch(/\u001b\[38[;:][0-9;:]*mhello/)
    expect(plain(rows[3]!)).toBe('')
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
  // colours; `COLORTERM` is what chalk reads as truecolour there.
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
  // Reasoning is a dim, slanted paragraph at the rail with no verb, so it reads
  // as the working-out rather than as output or as the answer.
  expect(frame).not.toContain('think')
  expect(frame).toContain('  \u001b[3m\u001b[2mCheck the files.')
  // The verb opens an action in bold; markers carry the outcome colours.
  expect(frame).toContain('\u001b[1mran\u001b[22m')
  expect(frame).toContain('\u001b[1m\u001b[38;2;34;197;94m\u25cf')
  expect(frame).toContain('\u001b[38;2;239;68;68m\u25cf')
  expect(frame).toContain('\u001b[1m\u001b[38;2;249;115;22m\u25cf')
  expect(frame).not.toContain('\u001b[2mran')
  // An edit says its size in each side's tone, numbers its lines in the
  // gutter, and reverses the words it changed.
  expect(frame).toContain('\u001b[38;2;34;197;94m  +1\u001b[39m \u001b[38;2;239;68;68m−1')
  expect(frame).toContain('\u001b[38;2;239;68;68m     4 - const a = \u001b[7mold')
  expect(frame).toContain('\u001b[38;2;34;197;94m     4 + const a = \u001b[7mnew')
  await expect(frame.replaceAll('\u001b', '<ESC>') + '\n').toMatchFileSnapshot('./expected/styles.txt')
})

it('sweeps the light along the rule through the ramp, and colours a cache-hit reading by how good it is', () => {
  // A child for its own colour environment, as above.
  const env: NodeJS.ProcessEnv = { ...process.env, FORCE_COLOR: '3', COLORTERM: 'truecolor' }
  delete env.NO_COLOR
  const frame = execFileSync(process.execPath, ['--import', 'tsx/esm', '--input-type=module', '--eval', `
    import React from 'react';
    import { renderToString } from 'ink';
    import { Rule, StatusBar } from ${JSON.stringify(new URL('../src/line.tsx', import.meta.url).href)};
    import { Beat } from ${JSON.stringify(new URL('../src/beat.tsx', import.meta.url).href)};
    import { FRAME_MS } from ${JSON.stringify(new URL('../src/activity.ts', import.meta.url).href)};
    import { cacheTone, PALETTE } from ${JSON.stringify(new URL('../src/palette.ts', import.meta.url).href)};
    // Four beats in, three cells a beat along a 100-column rule.
    const clock = { now: () => 4 * FRAME_MS, every: () => () => {} };
    const field = hit => ({ label: 'cache hit', value: hit + '%', color: cacheTone(hit) });
    process.stdout.write(renderToString(React.createElement(React.Fragment, null,
      React.createElement(Beat, { clock },
        React.createElement(Rule, { columns: 100, frame: 'round', clock,
          state: { kind: 'running', word: 'Working', phase: undefined, startedAt: 0, color: PALETTE.running } })),
      ...[85, 48, 12].map(hit => React.createElement(StatusBar, { key: hit,
        left: [{ text: 'Model: m' }, 'plan'],
        right: [{ text: 'Context: ~500/128k (0%)' }, 'in 1k', 'out 100', field(hit), '/w'], columns: 100 }))),
      { columns: 100 }));
  `], { cwd: new URL('../../../../../', import.meta.url), env, encoding: 'utf8', timeout: 20_000 })
  const rgb = (hex: string) => `\u001b[38;2;${[1, 3, 5].map(index => Number.parseInt(hex.slice(index, index + 2), 16)).join(';')}m`
  // The band rises through the ramp to the glint at its centre and falls
  // again, a cell a level, in the heavier glyph; the rest of the line is dim,
  // and the word keeps the running orange.
  const [base, low, high, glint] = ACCENT.ramp.map(rgb)
  const rule = frame.split('\n')[0]!
  expect(rule.replace(/\u001b\[[0-9;]*m/g, '')).toMatch(new RegExp(`^─ [\u2800-\u283f]{3} Working…  0s ${'─'.repeat(8)}${'━'.repeat(5)}${'─'.repeat(68)}$`))
  expect(rule).toContain(`${base}Working…`)
  expect(Array.from(rule.matchAll(/(\u001b\[38;2;[0-9;]+m)━/g), match => match[1])).toEqual([low, high, glint, high, low])
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
