/** Bun's hot component preview renders and accepts input in a real CI terminal. */
import { copyFile, mkdtemp, rm } from 'node:fs/promises'
import { join, resolve } from 'node:path'
import { tmpdir } from 'node:os'
import { expect, test } from 'bun:test'
import xterm from '@xterm/headless'
import { dictionaries } from '../../ui/src/copy.ts'

const harness = resolve(import.meta.dirname, '../../harness')

for (const locale of ['en', 'zh'] as const) test.skipIf(process.platform !== 'darwin' && process.platform !== 'linux')(
  `previews submissions and resizes a fixture with CI enabled (${locale})`, async () => {
    const copy = dictionaries[locale]
    const root = await mkdtemp(join(tmpdir(), 'bake-preview-'))
    const screen = new xterm.Terminal({ cols: 100, rows: 30, allowProposedApi: true })
    try {
      const fixture = join(root, 'recorded session.jsonl')
      await copyFile(join(harness, 'fixtures/session.jsonl'), fixture)
      let output = ''
      let pending = ''
      const decoder = new TextDecoder()
      const child = Bun.spawn([process.execPath, '--hot', join(harness, 'dev.tsx'), fixture,
        '--locale', locale, ...locale === 'zh' ? ['--replay'] : []], {
        cwd: root, env: { ...process.env, CI: '1', NO_COLOR: '1', TERM: 'xterm-256color' },
        timeout: 15_000,
        terminal: {
          cols: 100, rows: 30,
          data(_terminal, bytes) {
            const chunk = decoder.decode(bytes, { stream: true })
            output = (output + chunk).slice(-65_536)
            pending += chunk
          },
        },
      })
      const see = async (text: string): Promise<void> => {
        const deadline = performance.now() + 10_000
        while (!output.includes(text)) {
          if (child.exitCode !== null || child.signalCode !== null || performance.now() >= deadline) {
            throw new Error(`Preview did not show ${JSON.stringify(text)}: ${output}`)
          }
          await Bun.sleep(10)
        }
      }
      const capture = async (): Promise<string[]> => {
        const bytes = pending
        pending = ''
        if (bytes !== '') await new Promise<void>(resolve => screen.write(bytes, resolve))
        return Array.from({ length: screen.rows }, (_, row) =>
          screen.buffer.active.getLine(screen.buffer.active.viewportY + row)?.translateToString(true) ?? '')
      }
      const painted = async (description: string, predicate: (lines: string[]) => boolean): Promise<string[]> => {
        const deadline = performance.now() + 10_000
        while (true) {
          const lines = await capture()
          if (predicate(lines)) return lines
          if (child.exitCode !== null || child.signalCode !== null || performance.now() >= deadline) {
            throw new Error(`Preview did not ${description}:\n${lines.join('\n')}`)
          }
          await Bun.sleep(10)
        }
      }
      try {
        await see(copy.previewHelp.split('\n')[0]!)
        await see('bun run dev:tui')
        await see('\u001b[?2004h')
        child.terminal!.write('preview-probe')
        await painted('paint input under the preview help', lines => lines.some(line => line.includes('preview-probe▌')))
        child.terminal!.write('\r')
        await painted('commit the preview message', lines => lines.some(line => line === '● preview-probe'))
        await see(copy.previewAccepted)
        // Let recorded rows finish before checking scrollback and resize.
        await see(`\u2713 ${copy.turnCompleted}`)
        for (const [cols, rows] of [[40, 4], [120, 40]]) {
          await capture()
          const beforeResize = output.length
          screen.resize(cols!, rows!)
          child.terminal!.resize(cols!, rows!)
          child.kill('SIGWINCH')
          // The input row, its frame's bottom edge under it, and Ink's cursor
          // row, with the status line between them when the height allows it.
          const lines = await painted(`repaint the composer at ${cols}x${rows}`, lines => {
            const input = lines.findIndex(line => line.includes('▌'))
            return output.length > beforeResize && input >= 0 && input <= rows! - 3
              && lines[input + 1] === `╰${'─'.repeat(cols! - 2)}╯`
              && lines.filter(line => line.startsWith('╭')).length === 1
          })
          if (rows === 4) expect(lines.join('\n')).toBe(
            await Bun.file(join(import.meta.dirname, 'expected', `preview-short.${locale}.txt`)).text())
        }
        const history = Array.from({ length: screen.buffer.active.length }, (_, row) =>
          screen.buffer.active.getLine(row)?.translateToString(true) ?? '').join('\n')
        expect(history.split('● preview-probe')).toHaveLength(2)
        expect(history.split(copy.previewAccepted)).toHaveLength(2)
        child.terminal!.write('\u001b[A')
        await painted('recall the submitted preview', lines => lines.some(line => line.includes('preview-probe▌')))
        child.terminal!.write('\u0003')
        const code = await child.exited
        expect(child.signalCode).toBeNull()
        expect(code).toBe(0)
        expect(output).toContain('\u001b[?2004l')
      } finally {
        if (child.exitCode === null && child.signalCode === null) child.kill('SIGKILL')
        await child.exited
        child.terminal?.close()
      }
    } finally {
      screen.dispose()
      await rm(root, { recursive: true, force: true })
    }
  }, 20_000,
)
