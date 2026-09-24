/** Render common tool results with the production highlighter and without colour. */
import { execFileSync } from 'node:child_process'
import stringWidth from 'string-width'
import { expect, it } from 'vitest'
import type { ToolResultView } from '@deepseek-ai/dsh-tools'

const scenes: readonly { tool: string, title: string, view: ToolResultView, failed?: boolean }[] = [
  { tool: 'read', title: 'Read src/app.ts', view: { card: 'read', path: 'src/app.ts', offset: 1, totalLines: 3, lines: [
    { number: 1, text: 'const title = "中文"' }, { number: 2, text: '// a comment stays readable' }, { number: 3, text: 'export default title' },
  ] } },
  { tool: 'grep', title: 'Grep title', view: { card: 'search', shape: 'matches', truncated: false, total: 1,
    files: [{ path: 'src/app.ts', matches: [{ lineNumber: 3, line: 'export default title' }] }] } },
  { tool: 'bash', title: 'inspect-json', view: { card: 'terminal', output: '{"ok":true,"count":3}', exitCode: 0 } },
  { tool: 'bash', title: 'check', view: { card: 'terminal', output: 'WARN src/app.ts:3\nPASS tests\nhttps://example.com/docs', exitCode: 0 } },
  { tool: 'custom', title: 'Snippet', view: { card: 'generic', content: [{ type: 'text', text: '```python\ndef hello():\n    return "ready"\n```' }] } },
  { tool: 'custom', title: 'Unknown', view: { card: 'generic', content: [{ type: 'text', text: '```unknown-language\nkeep **literal**\n```' }] } },
  { tool: 'bash', title: 'failed-check', failed: true, view: { card: 'terminal', output: '\x1b[32mERROR src/app.ts:3\x1b[0m', exitCode: 1 } },
]

function render(columns: number, color: boolean): string {
  const env: NodeJS.ProcessEnv = { ...process.env, COLORTERM: 'truecolor' }
  if (color) { env.FORCE_COLOR = '3'; delete env.NO_COLOR }
  else { env.NO_COLOR = '1'; delete env.FORCE_COLOR }
  return execFileSync(process.execPath, ['--import', 'tsx/esm', '--input-type=module', '--eval', `
    import React from 'react';
    import { renderToString } from 'ink';
    import { RowView } from ${JSON.stringify(new URL('../src/app.tsx', import.meta.url).href)};
    import { ToolCards } from ${JSON.stringify(new URL('../src/cards.ts', import.meta.url).href)};
    import { dictionaries } from ${JSON.stringify(new URL('../src/copy.ts', import.meta.url).href)};
    import { budgetFor } from ${JSON.stringify(new URL('../src/layout.ts', import.meta.url).href)};
    import { createSyntax } from ${JSON.stringify(new URL('../../app/src/syntax.ts', import.meta.url).href)};
    const syntax = createSyntax();
    try {
      await syntax.ready;
      const rows = ${JSON.stringify(scenes)}.map((scene, index) => {
        const cards = new ToolCards(() => ({ presentResult: () => scene.view }), dictionaries.en);
        cards.call(String(index), scene.tool, '{}');
        const card = cards.result(String(index), { content: [], isError: scene.failed === true });
        return { kind: 'tool-call', callId: String(index), tool: scene.tool, input: scene.title,
          result: { ok: scene.failed !== true, text: '', detail: card.detail } };
      });
      const budget = budgetFor({ columns: ${columns}, rows: 24 });
      const result = { lines: 4, unit: 'lines', more: 'more lines', code: syntax.highlight };
      process.stdout.write(renderToString(React.createElement(React.Fragment, null,
        ...rows.map((row, key) => React.createElement(RowView, { key, row, budget, result }))), { columns: ${columns} }));
    } finally { await syntax.close(); }
  `], { cwd: new URL('../../../../../', import.meta.url), env, encoding: 'utf8', timeout: 20_000 })
}

it.each([40, 80])('keeps syntax and result structure readable without colour at %i columns', async columns => {
  const frame = render(columns, false)
  expect(frame).not.toContain('\x1b[')
  expect(frame).toContain('const title = "中文"')
  expect(frame).toContain('export default title')
  expect(frame).toContain('keep **literal**')
  expect(frame).not.toContain('```')
  expect(frame.split('\n').every(line => stringWidth(line) <= columns)).toBe(true)
  await expect(frame + '\n').toMatchFileSnapshot(`./expected/tool-output.${columns}.txt`)
})

it('colours source and structured results while failures stay red', async () => {
  const frame = render(80, true)
  expect(frame).toContain('\x1b[38;2;96;165;250msrc/app.ts')
  expect(frame).toContain('\x1b[38;2;234;179;8mWARN')
  expect(frame).toContain('\x1b[38;2;34;197;94mPASS')
  const failure = frame.slice(frame.indexOf('failed-check'))
  expect(failure).toContain('\x1b[38;2;239;68;68mERROR src/app.ts:3')
  expect(failure).not.toContain('\x1b[32m')
  expect(frame).not.toContain('\x1b[2mconst')
  await expect(frame.replaceAll('\x1b', '<ESC>') + '\n').toMatchFileSnapshot('./expected/tool-output.styles.txt')
})
