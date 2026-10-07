#!/usr/bin/env bun
/** Drive the built Rust preview in a real PTY and check its return to the shell. */
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { mkdir, mkdtemp, readdir, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { basename, join, resolve } from 'node:path'
import { setTimeout as delay } from 'node:timers/promises'
import { parseArgs } from 'node:util'
import xterm from '@xterm/headless'

const root = resolve(import.meta.dir, '..')
const { values } = parseArgs({ options: { binary: { type: 'string' } } })
const binary = resolve(values.binary ?? join(root, 'rust/target/debug/bake-rs'))
class Preview {
  private readonly terminal = new xterm.Terminal({ cols: 80, rows: 24, allowProposedApi: true })
  private readonly pty: Bun.Terminal
  private readonly initialModes: readonly number[]
  private readonly process: Bun.Subprocess
  private readonly exited: Promise<number>
  private parsed = Promise.resolve()
  private code: number | undefined
  private streamStatus: number | undefined
  private raw = ''

  constructor(cwd: string) {
    const decoder = new TextDecoder()
    // The shell waits for our handshake before exec, preserving its PID. This
    // lets us read cooked modes before the native process can enable raw mode.
    this.process = Bun.spawn(['sh', '-c', 'read -r ready; exec "$1" preview', 'rust-preview-pty', binary], {
      cwd,
      env: { PATH: process.env.PATH, TERM: 'xterm-256color', LANG: 'C.UTF-8' },
      terminal: {
        cols: 80, rows: 24,
        data: (_terminal, bytes) => {
          const text = decoder.decode(bytes, { stream: true })
          this.raw = (this.raw + text).slice(-1_048_576)
          this.parsed = this.parsed.then(() => new Promise<void>(done => this.terminal.write(text, done)))
        },
        exit: (_terminal, status) => { this.streamStatus = status },
      },
    })
    const pty = this.process.terminal
    assert(pty)
    this.pty = pty
    this.initialModes = this.modes()
    this.pty.write('start\n')
    this.exited = this.process.exited.then((code) => { this.code = code; return code })
  }

  private modes(): readonly number[] {
    return [this.pty.inputFlags, this.pty.outputFlags, this.pty.localFlags, this.pty.controlFlags]
  }

  get screen(): string {
    const buffer = this.terminal.buffer.active
    return Array.from({ length: this.terminal.rows }, (_, row) => buffer.getLine(buffer.viewportY + row)?.translateToString(true) ?? '').join('\n')
  }

  async wait(label: string, condition: () => boolean): Promise<void> {
    const deadline = performance.now() + 30_000
    while (true) {
      await this.parsed
      if (condition()) return
      if ((this.code !== undefined && this.streamStatus !== undefined) || performance.now() > deadline) {
        throw new Error(`${label}: ${this.code === undefined ? 'timed out' : `exited ${this.code}`}\n${this.screen}`)
      }
      await delay(5)
    }
  }

  async ready(): Promise<void> {
    await this.wait('Rust preview ready', () => this.terminal.buffer.active.type === 'alternate'
      // The sample session's newest lines and the composer show on the first frame.
      && this.screen.includes('▎ Run the parser tests') && this.screen.includes('❯ Type a draft')
      && this.raw.includes('\x1b[?2004h'))
    assert.notDeepEqual(this.modes(), this.initialModes, 'preview did not acquire raw mode')
  }

  send(text: string): void { this.pty.write(text) }

  async resize(cols: number, rows: number): Promise<void> {
    const offset = this.raw.length
    this.terminal.resize(cols, rows)
    this.pty.resize(cols, rows)
    await this.wait('resize redraw', () => this.raw.slice(offset).includes('\x1b[2J'))
  }

  async resizeBurst(): Promise<void> {
    this.process.kill('SIGSTOP')
    const offset = this.raw.length
    try {
      // Hold every application thread while the emulator reflows twice. The
      // final kernel size equals the previous frame, but that frame is stale.
      await this.wait('preview stopped', () => /^State:\s+T/mu.test(readFileSync(`/proc/${this.process.pid}/status`, 'utf8')))
      for (const [cols, rows] of [[40, 12], [80, 24]] as const) {
        this.terminal.resize(cols, rows)
        this.pty.resize(cols, rows)
      }
    } finally { this.process.kill('SIGCONT') }
    await this.wait('coalesced resize redraw', () => this.raw.slice(offset).includes('\x1b[2J'))
  }

  async quit(signal?: 'SIGINT' | 'SIGTERM' | 'SIGHUP'): Promise<void> {
    if (signal === 'SIGTERM') {
      this.send('\t')
      await this.wait('agent list with hidden cursor', () => this.screen.includes('Fixed examples · nothing is running')
        && this.raw.lastIndexOf('\x1b[?25l') > this.raw.lastIndexOf('\x1b[?25h'))
    }
    if (signal === undefined) this.send('\x03')
    else this.process.kill(signal)
    await this.wait('preview process and PTY exit', () => this.code !== undefined && this.streamStatus !== undefined)
    assert.equal(await this.exited, signal === 'SIGTERM' ? 143 : signal === 'SIGHUP' ? 129 : signal === 'SIGINT' ? 130 : 0)
    assert.equal(this.process.signalCode, null)
    // Linux reports EIO when the last slave closes, including a clean exit.
    assert(this.streamStatus === 0 || (process.platform === 'linux' && this.streamStatus === 1))
    assert.deepEqual(this.modes(), this.initialModes, 'terminal mode flags were not restored')
    assert.equal(this.terminal.buffer.active.type, 'normal', 'alternate screen was not released')
    assert(this.raw.lastIndexOf('\x1b[?2004l') > this.raw.lastIndexOf('\x1b[?2004h'), 'paste mode was not released')
    assert(this.raw.lastIndexOf('\x1b[?7h') > this.raw.lastIndexOf('\x1b[?7l'), 'autowrap was not restored')
    assert(this.raw.lastIndexOf('\x1b[?2026l') > this.raw.lastIndexOf('\x1b[?2026h'), 'synchronized output was left open')
    assert(this.raw.lastIndexOf('\x1b[?25h') > this.raw.lastIndexOf('\x1b[?25l'), 'cursor was not restored')
    assert.throws(() => process.kill(this.process.pid, 0), 'application remains alive after exit')
  }

  async close(): Promise<void> {
    if (this.code === undefined) this.process.kill('SIGKILL')
    await this.exited
    this.pty.close()
    await this.parsed
    this.terminal.dispose()
  }

  async saveFailure(path: string): Promise<void> { await writeFile(path, this.raw) }
}

async function scenario(name: string, run: (preview: Preview) => Promise<void>): Promise<void> {
  const cwd = await mkdtemp(join(tmpdir(), 'bake-rust-pty-'))
  let preview: Preview | undefined
  try {
    preview = new Preview(cwd)
    await preview.ready()
    await run(preview)
    assert.deepEqual(await readdir(cwd), [], 'preview wrote files into the working directory')
    console.log(`PASS Rust PTY: ${name}`)
  } catch (error) {
    if (preview !== undefined) {
      await mkdir(join(root, '.preflight'), { recursive: true })
      const transcript = join(root, '.preflight', `${basename(cwd)}.ansi`)
      await preview.saveFailure(transcript)
      console.error(`Rust PTY transcript: ${transcript}`)
    }
    throw error
  } finally {
    await preview?.close()
    await rm(cwd, { recursive: true })
  }
}

if (process.platform === 'win32') throw new Error('Rust PTY scenarios require POSIX; ConPTY qualification remains open')

await scenario('composer, agent inspection, paste, and resize', async (preview) => {
  preview.send('draftAB\x1b[D')
  await preview.wait('typed draft', () => preview.screen.includes('draftAB'))
  preview.send('\t')
  await preview.wait('sample agent picker', () => preview.screen.includes('Fixed examples · nothing is running'))
  preview.send('\x1b[B\r')
  await preview.wait('read-only inspection', () => preview.screen.includes('typing never reaches an agent'))
  preview.send('forbidden\x1b')
  await preview.wait('return to composer', () => !preview.screen.includes('typing never reaches an agent') && preview.screen.includes('draftAB'))
  preview.send('X')
  await preview.wait('restored caret', () => preview.screen.includes('draftAXB'))
  assert(!preview.screen.includes('forbidden'), 'inspection accepted draft input')
  preview.send('\x1a')
  await preview.wait('undo after inspection', () => preview.screen.includes('draftAB') && !preview.screen.includes('draftAXB'))
  preview.send('\r')
  await preview.wait('refused submission', () => /not available/iu.test(preview.screen))
  assert(preview.screen.includes('draftAB'), 'refused submission cleared the draft')
  preview.send('\x1b[200~line one\r\nline two\x1b[201~')
  await preview.wait('multiline paste', () => preview.screen.includes('line one') && preview.screen.includes('line two'))
  await preview.resize(40, 12)
  await preview.wait('narrow draft', () => preview.screen.includes('line two'))
  await preview.resize(80, 24)
  await preview.wait('resized draft', () => preview.screen.includes('line one') && preview.screen.includes('line two'))
  preview.send('\x1a')
  await preview.wait('atomic paste undo', () => preview.screen.includes('draftAB') && !preview.screen.includes('line one'))
  await preview.quit()
})

await scenario('transcript pages, jumps between prompts, and follows output again', async (preview) => {
  preview.send('keep')
  await preview.wait('following output', () => preview.screen.includes('PgUp scroll · Ctrl+↑ prompts') && preview.screen.includes('before splitting.'))
  preview.send('\x1b[5~')
  await preview.wait('reading history', () => preview.screen.includes('↓ Latest · Ctrl+End') && !preview.screen.includes('before splitting.'))
  // One prompt back from the newest lines is the code-mode turn: its program, its tree of calls, and what it returned.
  preview.send('\x1b[1;5F\x1b[1;5A')
  await preview.wait('code-mode block', () => /^▎ Find TODO comments/u.test(preview.screen)
    && /✓ Script {2}Find TODOs +13 calls · 1 failed/u.test(preview.screen)
    && /✗ Read {2}src\/m5\.ts +Permission denied/u.test(preview.screen))
  preview.send('\x1b[1;5H')
  await preview.wait('transcript start', () => /^ {2}Bake · Rust preview/mu.test(preview.screen))
  preview.send('\x1b[1;5B')
  await preview.wait('next prompt at the top', () => /^▎ Find where the session controller/u.test(preview.screen))
  preview.send('\x1b[1;5F')
  await preview.wait('following again', () => preview.screen.includes('PgUp scroll') && preview.screen.includes('before splitting.'))
  assert(preview.screen.includes('❯ keep'), 'transcript navigation changed the draft')
  await preview.quit()
})

await scenario('sample activity is text that advances on its own, then compacts, and stops', async (preview) => {
  preview.send('\x14')
  await preview.wait('sample turn', () => /^ {2}\S+… {2}thinking · 0s +no model/mu.test(preview.screen) && preview.screen.includes('Esc interrupts')
    && preview.screen.includes('Enter steers the next step'))
  // No key is pressed: the loop's own timer must redraw the elapsed time.
  await preview.wait('elapsed time advances', () => preview.screen.includes('thinking · 1s'))
  assert(!/[\u2800-\u28ff]/u.test(preview.screen), 'the activity drew a spinner glyph')
  preview.send('\x14')
  await preview.wait('sample compaction', () => preview.screen.includes('Compacting history…  preparing · 0s')
    && preview.screen.includes('Compacting… Enter queues · Esc cancels') && !preview.screen.includes('Esc interrupts'))
  preview.send('\x1b')
  // The compaction ends. The bar keeps the turn's outcome on its left and the
  // status, no model and then the directory, right-aligned on the same row.
  await preview.wait('sample stopped', () => /^ {2}✓ Completed {2}\d+s +no model {2}\S+$/mu.test(preview.screen)
    && !preview.screen.includes('Compacting'))
  await preview.quit()
})

if (process.platform === 'linux') {
  await scenario('coalesced resize invalidates stale screen', async (preview) => {
    preview.send('RESIZE_DRAFT')
    await preview.wait('draft before resize', () => preview.screen.includes('RESIZE_DRAFT'))
    await preview.resizeBurst()
    await preview.wait('draft after resize', () => preview.screen.includes('RESIZE_DRAFT'))
    assert.equal(preview.screen.split('RESIZE_DRAFT').length - 1, 1, 'resize duplicated the draft')
    await preview.quit()
  })
} else console.log('SKIP Rust PTY: coalesced resize barrier needs Linux /proc process state')

for (const signal of ['SIGINT', 'SIGTERM', 'SIGHUP'] as const) {
  await scenario(`${signal} restores terminal`, async (preview) => { await preview.quit(signal) })
}
