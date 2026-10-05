/** Render common tool results with the production highlighter and without colour. */
import { execa } from 'execa'
import stringWidth from 'string-width'
import { expect, it, onTestFinished } from 'vitest'
import type { ToolResultView } from 'bake-tools'

type Scene = { tool: string, title: string, view: ToolResultView, failed?: boolean }

const scenes: readonly Scene[] = [
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

/** Commands that changed files, and an edit whose file holds terminal controls. */
const changeScenes: readonly Scene[] = [
  { tool: 'bash', title: "sed -i 's/= 3/= 5/' a.js && cp t b.js && node t.cjs", failed: true, view: { card: 'terminal', exitCode: 1,
    output: ['TAP version 13', 'not ok 1 - retries', ...Array.from({ length: 6 }, (_, index) => `  # detail ${index}`), '# fail 1'].join('\n'),
    changes: { files: [
      { path: 'src/a.js', status: 'modified', added: 1, removed: 1, hunks: [{ path: 'src/a.js', oldText: 'const retries = 3\n', newText: 'const retries = 5\n', oldStart: 4, newStart: 4 }] },
      { path: 'b.js', status: 'created', added: 1, removed: 0, hunks: [{ path: 'b.js', oldText: null, newText: 'module.exports = {}\n', newStart: 1 }] },
    ] } } },
  { tool: 'bash', title: 'python3 rewrite.py', view: { card: 'terminal', exitCode: 0, changes: { concurrent: true, files: [
    { path: 'logo.png', status: 'binary', added: 0, removed: 0 },
    { path: 'docs/new-name.md', status: 'renamed', from: 'docs/old-name.md', added: 0, removed: 0 },
    { path: 'gen/schema.json', status: 'too-large', added: 0, removed: 0 },
    { path: 'notes.txt', status: 'deleted', added: 0, removed: 2, hunks: [{ path: 'notes.txt', oldText: 'first\nsecond\n', newText: '', oldStart: 1, newStart: 1 }] },
  ] } } },
  { tool: 'bash', title: 'npm run format', view: { card: 'terminal', output: 'formatted 9 files', exitCode: 0, changes: { timedOut: true, omittedFiles: 2,
    files: Array.from({ length: 7 }, (_, index) => ({ path: `src/f${index}.ts`, status: 'modified' as const, added: 1, removed: 1,
      hunks: [{ path: `src/f${index}.ts`, oldText: `let v${index} = 1\n`, newText: `let v${index} = 2\n`, oldStart: 1, newStart: 1 }] })) } } },
  { tool: 'bash', title: 'printf ...', view: { card: 'terminal', exitCode: 0, changes: { files: [{ path: 'raw.txt', status: 'modified', added: 2, removed: 1,
    hunks: [{ path: 'raw.txt', oldText: 'level\tone of the config\r\n', newText: '\x1b[31mlevel\x1b[0m\ttwo of the config\r\nbell\x07\rback\r\n', oldStart: 1, newStart: 1 }] }] } } },
  { tool: 'edit', title: 'Edit raw.txt', view: { card: 'diff', title: 'Edit raw.txt', diffs: [
    { path: 'raw.txt', oldText: 'level\tone of the config\n', newText: '\x1b[1mlevel\x1b[0m\ttwo of the config\n', oldStart: 1, newStart: 1 },
  ] } },
]

async function render(columns: number, color: boolean, list: readonly Scene[] = scenes): Promise<string> {
  const env: NodeJS.ProcessEnv = { ...process.env, COLORTERM: 'truecolor' }
  if (color) { env.FORCE_COLOR = '3'; delete env.NO_COLOR }
  else { env.NO_COLOR = '1'; delete env.FORCE_COLOR }
  const child = execa(process.execPath, ['--import', 'tsx/esm', '--input-type=module', '--eval', `
    import React from 'react';
    import { renderToString } from 'ink';
    import { RowView } from ${JSON.stringify(new URL('../src/app.tsx', import.meta.url).href)};
    import { ToolCards } from ${JSON.stringify(new URL('../src/cards.ts', import.meta.url).href)};
    import { dictionaries } from ${JSON.stringify(new URL('../src/copy.ts', import.meta.url).href)};
    import { budgetFor } from ${JSON.stringify(new URL('../src/layout.ts', import.meta.url).href)};
    import { createSyntax } from ${JSON.stringify(new URL('../../app/src/syntax.ts', import.meta.url).href)};
    const syntax = createSyntax(undefined, { tokenizeTimeLimit: 0 });
    try {
      await syntax.ready;
      const rows = ${JSON.stringify(list)}.map((scene, index) => {
        const cards = new ToolCards(() => ({ presentResult: () => scene.view }), dictionaries.en);
        cards.call(String(index), scene.tool, '{}');
        const card = cards.result(String(index), { content: [], isError: scene.failed === true });
        return { kind: 'tool-call', callId: String(index), tool: scene.tool, input: scene.title,
          result: { ok: scene.failed !== true, text: '', detail: card.detail, ...card.changes === undefined ? {} : { changes: card.changes } } };
      });
      const budget = budgetFor({ columns: ${columns}, rows: 24 });
      const { cardFiles: files, moreFiles, moreFile } = dictionaries.en;
      const result = { lines: 4, unit: 'lines', more: 'more lines', files, moreFiles, moreFile, code: syntax.highlight };
      process.stdout.write(renderToString(React.createElement(React.Fragment, null,
        ...rows.map((row, key) => React.createElement(RowView, { key, row, budget, frame: 'classic', result }))), { columns: ${columns} }));
    } finally { await syntax.close(); }
  `], { cwd: new URL('../../../../../', import.meta.url), env })
  onTestFinished(async () => { child.kill('SIGKILL'); await child.catch(() => {}) })
  return (await child).stdout
}

it.each([40, 80])('keeps syntax and result structure readable without colour at %i columns', async columns => {
  const frame = await render(columns, false)
  expect(frame).not.toContain('\x1b[')
  expect(frame).toContain('const title = "中文"')
  expect(frame).toContain('export default title')
  expect(frame).toContain('keep **literal**')
  expect(frame).not.toContain('```')
  expect(frame.split('\n').every(line => stringWidth(line) <= columns)).toBe(true)
  await expect(frame + '\n').toMatchFileSnapshot(`./expected/tool-output.${columns}.txt`)
})

it('colours source and structured results while failures stay red', async () => {
  const frame = await render(80, true)
  expect(frame).toContain('\x1b[38;2;96;165;250msrc/app.ts')
  expect(frame).toContain('\x1b[38;2;234;179;8mWARN')
  expect(frame).toContain('\x1b[38;2;34;197;94mPASS')
  const failure = frame.slice(frame.indexOf('failed-check'))
  expect(failure).toContain('\x1b[38;2;239;68;68mERROR src/app.ts:3')
  expect(failure).not.toContain('\x1b[32m')
  expect(frame).not.toContain('\x1b[2mconst')
  await expect(frame.replaceAll('\x1b', '<ESC>') + '\n').toMatchFileSnapshot('./expected/tool-output.styles.txt')
})

it.each([40, 80])('draws a command\'s changed files under its output at %i columns', async columns => {
  const frame = await render(columns, false, changeScenes)
  expect(frame).not.toContain('\x1b')
  expect(frame).not.toContain('\r')
  expect(frame).not.toContain('\t')
  expect(frame).toContain('edited')
  expect(frame.split('\n').every(line => stringWidth(line) <= columns)).toBe(true)
  await expect(frame + '\n').toMatchFileSnapshot(`./expected/tool-changes.${columns}.txt`)
})

it('keeps a failed command\'s changes in their own tones', async () => {
  const frame = await render(80, true, changeScenes)
  const failed = frame.slice(0, frame.indexOf('python3'))
  // The output is red, and the diff under it is red and green by side, not red throughout.
  expect(failed).toContain('\x1b[38;2;239;68;68mnot ok 1 - retries')
  expect(failed).toContain('\x1b[38;2;34;197;94m     4 + const')
  expect(failed).toContain('\x1b[1medited\x1b[22m \x1b[38;2;96;165;250msrc/a.js')
  await expect(frame.replaceAll('\x1b', '<ESC>') + '\n').toMatchFileSnapshot('./expected/tool-changes.styles.txt')
})
