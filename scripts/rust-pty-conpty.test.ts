import { mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { describe, expect, it } from 'bun:test'
import {
  ConPty, consoleSignal, eventModes, LineSplitter, modeRestoration, parseEvent, rawModeProblem, STATUS_CONTROL_C_EXIT, VT_PROCESSING,
  windowsEnv,
} from './rust-pty-conpty.ts'

const bytes = (text: string) => new TextEncoder().encode(text)

describe('ConPTY driver helpers', () => {
  it('splits LF and CRLF lines, keeping a line and a character split across chunks whole', () => {
    const lines = new LineSplitter()
    const wide = bytes('{"event":"漢"}\r\n')
    expect(lines.push(bytes('{"a":1}\r\n{"b"'))).toEqual(['{"a":1}'])
    expect(lines.push(bytes(':2}\n\r\n'))).toEqual(['{"b":2}'])
    // The first byte of 漢 ends one chunk; the rest starts the next.
    expect(lines.push(wide.slice(0, 11))).toEqual([])
    expect(lines.push(wide.slice(11))).toEqual(['{"event":"漢"}'])
    expect(lines.push(bytes('{"tail":true}\r'))).toEqual([])
    expect(lines.end()).toEqual(['{"tail":true}'])
    expect(lines.end()).toEqual([])
  })

  it('reads events and refuses lines that are not event objects', () => {
    expect(parseEvent('{"event":"modes","phase":"initial","input":503,"output":7}').event).toBe('modes')
    expect(eventModes(parseEvent('{"event":"modes","input":503,"output":7}'))).toEqual({ input: 503, output: 7 })
    for (const line of ['garbage', '[1]', 'null', '{"op":"write"}', '{"event":3}'])
      expect(() => parseEvent(line)).toThrow(/ConPTY host wrote a line/u)
    expect(() => eventModes(parseEvent('{"event":"modes","input":1.5,"output":7}'))).toThrow(/malformed modes/u)
  })

  it('requires cooked input before the program and every cooked bit clear while it runs', () => {
    // 0x1f7: conhost's default input mode, cooked; 0x1f0 with the cooked bits cleared.
    expect(rawModeProblem({ input: 0x1f7, output: 3 }, { input: 0x1f0, output: 7 })).toBeUndefined()
    expect(rawModeProblem({ input: 0x1f7, output: 3 }, { input: 0x1f1, output: 7 })).toMatch(/did not acquire raw mode/u)
    expect(rawModeProblem({ input: 0x1f0, output: 3 }, { input: 0x1f0, output: 7 })).toMatch(/not cooked before/u)
  })

  it('requires the input mode restored exactly and names only the VT-processing difference in the output mode as a gap', () => {
    const before = { input: 0x1f7, output: 0x3 }
    expect(modeRestoration(before, before)).toEqual({})
    expect(modeRestoration(before, { input: 0x1f0, output: 0x3 }).problem).toMatch(/input mode 0x1f0 was not restored to 0x1f7/u)
    expect(modeRestoration(before, { input: 0x1f7, output: 0x3 | VT_PROCESSING }).gap).toMatch(/ENABLE_VIRTUAL_TERMINAL_PROCESSING/u)
    expect(modeRestoration({ input: 0x1f7, output: 0x7 }, { input: 0x1f7, output: 0x3 }).problem).toMatch(/output mode/u)
    expect(modeRestoration(before, { input: 0x1f7, output: 0xb }).problem).toMatch(/output mode 0xb/u)
  })

  it('maps the scenarios’ signals to console events, a closed pseudoconsole, and termination', () => {
    expect(consoleSignal('SIGINT')).toEqual({ op: 'ctrl', event: 'c' })
    expect(consoleSignal('SIGTERM')).toEqual({ op: 'ctrl', event: 'break' })
    expect(consoleSignal('SIGHUP')).toEqual({ op: 'hangup' })
    expect(consoleSignal('SIGKILL')).toEqual({ op: 'kill' })
    expect(STATUS_CONTROL_C_EXIT).toBe(3_221_225_786)
  })

  it('passes each Windows variable once whatever its case, the scenario’s own winning', () => {
    const env = windowsEnv(
      { Path: 'C:\\Windows', SYSTEMROOT: 'C:\\Windows', TEMP: 'C:\\T', 'ProgramFiles(x86)': 'C:\\P86', SECRET: 'no', TERM: 'dumb' },
      { TERM: 'xterm-256color', home: 'C:\\h', HOME: 'C:\\Users\\ü b' },
    )
    expect(env).toEqual({
      PATH: 'C:\\Windows', SystemRoot: 'C:\\Windows', TEMP: 'C:\\T', 'ProgramFiles(x86)': 'C:\\P86', TERM: 'xterm-256color', HOME: 'C:\\Users\\ü b',
    })
    expect(Object.keys(env).map(key => key.toUpperCase()).length).toBe(new Set(Object.keys(env).map(key => key.toUpperCase())).size)
  })
})

/** A stand-in host speaking the protocol, so the client is tested where no pseudoconsole exists. */
const STAND_IN = String.raw`
const out = (event) => process.stdout.write(JSON.stringify(event) + '\r\n')
const early = process.argv.includes('--early-exit')
out({ event: 'started', client: 10 })
out({ event: 'modes', phase: 'initial', input: 0x1f7, output: 3 })
out({ event: 'spawned', pid: 11 })
if (early) { out({ event: 'error', message: 'host: CreatePseudoConsole: HRESULT 0x80070057' }); process.exit(1) }
let rest = ''
for await (const chunk of process.stdin) {
  rest += chunk
  const lines = rest.split('\n'); rest = lines.pop()
  for (const line of lines) {
    const command = JSON.parse(line)
    if (command.op === 'write') out({ event: 'output', data: command.data })
    if (command.op === 'modes') out({ event: 'modes', phase: 'running', input: 0x1f0, output: 7, id: command.id })
    if (command.op === 'members') out({ event: 'members', id: command.id, members: [{ pid: 12, role: 'member', image: 'C:\\Git\\usr\\bin\\sleep.exe', alive: true }] })
    if (command.op === 'close') {
      out({ event: 'modes', phase: 'final', input: 0x1f7, output: 7 })
      out({ event: 'exit', code: 0 })
      out({ event: 'client-exit', code: 0 })
      out({ event: 'eof' })
      out({ event: 'settled', terminated: false })
      out({ event: 'closed' })
    }
  }
}
`

describe('ConPty against a stand-in host', () => {
  const standIn = async () => {
    const dir = await mkdtemp(join(tmpdir(), 'bake-conpty-stand-in-'))
    const script = join(dir, 'host.mjs')
    await writeFile(script, STAND_IN)
    return { dir, script }
  }

  it('relays bytes, answers requests by id, and reads the exit, final modes, and settlement', async () => {
    const { dir, script } = await standIn()
    const received: string[] = []
    const pty = new ConPty({
      host: [process.execPath, script], program: 'bake.exe', args: ['--inline'], cwd: dir, env: { PATH: process.env.PATH ?? '' },
      cols: 80, rows: 24, history: true, onData: data => received.push(Buffer.from(data).toString('utf8')),
    })
    try {
      expect(await pty.initial).toEqual({ input: 0x1f7, output: 3 })
      pty.write('漢\x1b[A')
      expect(await pty.modes()).toEqual({ input: 0x1f0, output: 7 })
      expect(received.join('')).toBe('漢\x1b[A')
      expect(await pty.members()).toEqual([{ pid: 12, role: 'member', image: 'C:\\Git\\usr\\bin\\sleep.exe', alive: true }])
      expect(pty.pid).toBe(11)
      await pty.close()
      expect(await pty.exited).toBe(0)
      expect(await pty.final).toEqual({ input: 0x1f7, output: 7 })
      expect(pty.closed).toBe(true)
      expect(pty.settled).toEqual({ terminated: false })
      expect(pty.errors).toEqual([])
      expect(pty.hostExitCode).toBe(0)
      expect(() => pty.write('late')).toThrow(/has closed/u)
    } finally {
      await rm(dir, { recursive: true, force: true })
    }
  })

  it('fails every wait with the host’s error when it exits before closing', async () => {
    const { dir, script } = await standIn()
    const pty = new ConPty({
      host: [process.execPath, script, '--early-exit'], program: 'bake.exe', args: [], cwd: dir, env: { PATH: process.env.PATH ?? '' },
      cols: 80, rows: 24, history: false, onData: () => {},
    })
    try {
      expect(await pty.initial).toEqual({ input: 0x1f7, output: 3 })
      await expect(pty.exited).rejects.toThrow(/exited 1 before it closed/u)
      await expect(pty.ended).rejects.toThrow(/exited 1 before it closed/u)
      expect(pty.errors[0]).toBe('host: CreatePseudoConsole: HRESULT 0x80070057')
      await expect(pty.modes()).rejects.toThrow()
    } finally {
      await pty.close()
      await rm(dir, { recursive: true, force: true })
    }
  })
})
