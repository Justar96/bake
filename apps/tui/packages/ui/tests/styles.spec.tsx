/** ANSI emphasis for reasoning, actions, and their results. */
import { execFileSync } from 'node:child_process'
import { expect, it } from 'vitest'
import type { Row } from '../src/rows.ts'

it('dims reasoning and metadata, and gives actions and outcomes their weight and colour', async () => {
  const rows: Row[] = [
    { kind: 'reasoning', text: 'Check the files.\nThen verify the change.' },
    { kind: 'tool-call', callId: 'c1', tool: 'bash', input: 'ls -a', detail: [{ text: 'List the directory' }],
      result: { ok: true, text: 'one.ts\ntwo.ts\nthree.ts\nfour.ts' } },
    { kind: 'tool-call', callId: 'c2', tool: 'str_replace_editor', input: 'one.ts', result: { ok: true, text: '', title: 'Edit one.ts', detail: [
      { text: '  context' }, { text: '- old', emphasis: 'removed' }, { text: '+ new', emphasis: 'added' },
    ] } },
    { kind: 'tool-call', callId: 'c3', tool: 'read_file', input: 'Read secret.md', result: { ok: false, text: 'Permission denied' } },
    { kind: 'tool-call', callId: 'c4', tool: 'grep', input: 'TODO' },
    { kind: 'assistant', text: 'Updated one.ts.' },
  ]
  // Chalk reads color support during module initialization; a child gives this
  // render its own environment without changing other Ink tests' styling.
  const env: NodeJS.ProcessEnv = { ...process.env, FORCE_COLOR: '1' }
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
  expect(frame).toContain('\u001b[2mthink\u001b[22m')
  // The verb opens an action in bold; a finished one's marker is green, a
  // failed one's red, and a running one's the accent.
  expect(frame).toContain('\u001b[1mran\u001b[22m')
  expect(frame).toContain('\u001b[1m\u001b[32m\u25cf')
  expect(frame).toContain('\u001b[31m\u25cf')
  expect(frame).toContain('\u001b[38;5;180m\u25cf')
  expect(frame).not.toContain('\u001b[2mran')
  await expect(frame.replaceAll('\u001b', '<ESC>') + '\n').toMatchFileSnapshot('./expected/styles.txt')
})
