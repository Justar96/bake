/** Native arm resolution, launch arguments, stale writer, and context-file guard, without a real Cargo build or a model. */
import { afterAll, describe, expect, test } from 'bun:test'
import { createHash } from 'node:crypto'
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { delimiter, join } from 'node:path'
import { builtExecutable, nativeCommand, nativeStaleWriter, parseArms, strayContextFiles } from './arms.ts'

const root = mkdtempSync(join(tmpdir(), 'bake-eval-arms-'))
afterAll(() => rmSync(root, { recursive: true, force: true }))
const sha256 = (text: string) => createHash('sha256').update(text).digest('hex')
const posix = process.platform !== 'win32'

/** An executable shell script; the cases that use one need a POSIX shell. */
function script(path: string, text: string): string {
  mkdirSync(join(path, '..'), { recursive: true })
  writeFileSync(path, text)
  chmodSync(path, 0o755)
  return path
}

describe('native arms', () => {
  test('launch print mode with JSON events, no session, the full-access preset, and the prompt after --', () => {
    expect(nativeCommand('/bin/bake-rs', 'cliproxyapi', 'gpt-x', 'medium', '-starts with a dash'))
      .toEqual(['/bin/bake-rs', '-p', '--mode', 'json', '--no-session', '--permission', 'danger-full-access',
        '--model', 'cliproxyapi/gpt-x', '--thinking', 'medium', '--', '-starts with a dash'])
  })

  test('run under the default preset when the evaluation asks for it', () => {
    expect(nativeCommand('/bin/bake-rs', 'cliproxyapi', 'gpt-x', 'medium', 'go', 'workspace-write'))
      .toEqual(['/bin/bake-rs', '-p', '--mode', 'json', '--no-session', '--permission', 'workspace-write',
        '--model', 'cliproxyapi/gpt-x', '--thinking', 'medium', '--', 'go'])
  })

  test('the executable is the bake-rs binary Cargo reports, wherever its target directory is', () => {
    const artifact = (name: string, kind: string, executable: string | null) =>
      JSON.stringify({ reason: 'compiler-artifact', target: { name, kind: [kind] }, executable })
    expect(builtExecutable([
      artifact('bake_coding_agent', 'lib', null), 'not json', artifact('bake-rs', 'bin', '/elsewhere/release/bake-rs'),
      JSON.stringify({ reason: 'build-finished', success: true }),
    ].join('\n'))).toBe('/elsewhere/release/bake-rs')
    expect(builtExecutable(artifact('bake-eval-fake-arm', 'bin', '/t/bake-eval-fake-arm'))).toBeUndefined()
  })

  test.skipIf(!posix)('a prebuilt binary is run as given and credited to no commit', () => {
    // Inside a directory that looks like a checkout, which must not lend the binary its commit.
    const checkout = join(root, 'prebuilt')
    mkdirSync(join(checkout, 'rust'), { recursive: true })
    writeFileSync(join(checkout, 'rust/Cargo.toml'), '[workspace]\n')
    const text = '#!/bin/sh\necho "bake-rs 9.9.9-test"\n'
    const bin = script(join(checkout, 'rust/target/release/bake-rs'), text)
    expect(parseArms(`cand=native:${bin}`, 'headless', {}).cand).toEqual({
      kind: 'native', root: realpathSync(join(checkout, 'rust/target/release')), bin: realpathSync(bin), version: 'bake-rs 9.9.9-test',
      binarySha256: sha256(text), revision: { prebuilt: true, commit: null, dirty: null },
      extra: [], settings: '{}', llmDeepseek: false, composition: [],
    })
  })

  test.skipIf(!posix)('a checkout runs the binary its locked release build reports, not a stale one under rust/target', () => {
    const checkout = join(root, 'checkout')
    mkdirSync(join(checkout, 'rust'), { recursive: true })
    writeFileSync(join(checkout, 'rust/Cargo.toml'), '[workspace]\n')
    script(join(checkout, 'rust/target/release/bake-rs'), '#!/bin/sh\necho "bake-rs stale"\n')
    const fresh = '#!/bin/sh\necho "bake-rs fresh"\n'
    const built = script(join(root, 'redirected-target/release/bake-rs'), fresh)
    const args = join(root, 'cargo-args')
    // A stand-in Cargo that records its arguments and directory and reports the redirected build.
    const bin = join(root, 'fake-bin')
    script(join(bin, 'cargo'), `#!/bin/sh\necho "$PWD $*" > ${JSON.stringify(args)}\n`
      + `echo '${JSON.stringify({ reason: 'compiler-artifact', target: { name: 'bake-rs', kind: ['bin'] }, executable: built })}'\n`)
    const path = process.env.PATH
    process.env.PATH = `${bin}${delimiter}${path ?? ''}`
    try {
      const arm = parseArms(`cand=native:${checkout}`, 'headless', {}).cand!
      expect(arm.bin).toBe(realpathSync(built))
      expect(arm.version).toBe('bake-rs fresh')
      expect(arm.binarySha256).toBe(sha256(fresh))
      expect(arm.root).toBe(checkout)
      expect(arm.revision?.prebuilt).toBe(false)
    } finally {
      process.env.PATH = path
    }
    expect(readFileSync(args, 'utf8').trim())
      .toBe(`${realpathSync(join(checkout, 'rust'))} build --locked --release -p bake-cli --message-format=json-render-diagnostics`)
  })

  test('a missing path or a directory without a Rust workspace fails before any sample', () => {
    expect(() => parseArms(`cand=native:${join(root, 'missing')}`, 'headless', {})).toThrow(/does not exist/)
    mkdirSync(join(root, 'plain'))
    expect(() => parseArms(`cand=native:${join(root, 'plain')}`, 'headless', {})).toThrow(/no rust\/Cargo.toml/)
  })
})

describe('the native stale writer', () => {
  /** A fresh sample directory with the target file. */
  function sample() {
    const dir = mkdtempSync(join(root, 'writer-'))
    const workspace = join(dir, 'workspace')
    mkdirSync(join(workspace, 'src'), { recursive: true })
    writeFileSync(join(workspace, 'src/money.js'), 'original\n')
    const injection = join(dir, 'injection.json')
    return { workspace, target: join(workspace, 'src/money.js'), injection, writer: nativeStaleWriter(workspace, join(workspace, 'src/money.js'), injection) }
  }
  let id = 0
  const response = () => ({ type: 'message_end', message: { role: 'assistant' } })
  const start = (toolName: string, args: Record<string, unknown>) => ({ type: 'tool_execution_start', toolCallId: `c${++id}`, toolName, args })
  const end = (started: { toolCallId: string; toolName: string }, isError = false) => ({ type: 'tool_execution_end', toolCallId: started.toolCallId, toolName: started.toolName, isError })

  test('prepends the kept comment once, after the first successful read of the target', () => {
    const { workspace, target, injection, writer } = sample()
    writer.onEvent(response())
    const other = start('read', { path: 'test.cjs' })
    const failed = start('read', { path: 'src/money.js' })
    for (const event of [other, end(other), failed, end(failed, true)]) writer.onEvent(event)
    expect(readFileSync(target, 'utf8')).toBe('original\n')
    expect(existsSync(injection)).toBe(false)
    writer.onEvent(response())
    const first = start('read', { path: 'src/money.js' })
    writer.onEvent(first)
    writer.onEvent(end(first))
    writer.onEvent(response())
    const again = start('read', { path: join(workspace, 'src/money.js') })
    writer.onEvent(again)
    writer.onEvent(end(again))
    writer.onEvent(response())
    writer.onEvent(start('edit', { path: 'src/money.js', edits: [] }))
    expect(readFileSync(target, 'utf8')).toBe('// EXTERNAL_CHANGE_KEEP\noriginal\n')
    expect(JSON.parse(readFileSync(injection, 'utf8'))).toEqual({ injected: true })
    expect(writer.state).toEqual({ injected: true, raced: false })
  })

  test('an edit in the same response as the injecting read is a raced injection, whichever starts first', () => {
    for (const editFirst of [true, false]) {
      const { writer } = sample()
      writer.onEvent(response())
      const read = start('read', { path: 'src/money.js' })
      const edit = start('edit', { path: 'src/money.js', edits: [] })
      const order = editFirst ? [read, edit, end(read)] : [read, end(read), edit]
      for (const event of order) writer.onEvent(event)
      expect(writer.state).toEqual({ injected: true, raced: true })
    }
  })

  test('a check in the same response as the read is not a race', () => {
    const { writer } = sample()
    writer.onEvent(response())
    const read = start('read', { path: 'src/money.js' })
    for (const event of [read, start('bash', { command: 'node test.cjs' }), end(read)]) writer.onEvent(event)
    expect(writer.state).toEqual({ injected: true, raced: false })
  })
})

describe('stray context files', () => {
  test('names a file in ~/.agents or any ancestor, and the workspace\'s own only when the scenario does not own it', () => {
    const dir = mkdtempSync(join(root, 'context-'))
    const workspace = join(dir, 'a/workspace')
    const userHome = join(dir, 'user')
    mkdirSync(workspace, { recursive: true })
    mkdirSync(join(userHome, '.agents'), { recursive: true })
    expect(strayContextFiles(workspace, userHome, false).filter(path => path.startsWith(realpathSync(dir)) || path.startsWith(userHome))).toEqual([])
    writeFileSync(join(userHome, '.agents/AGENTS.md'), 'global\n')
    writeFileSync(join(dir, 'a/AGENTS.md'), 'ancestor\n')
    writeFileSync(join(workspace, 'AGENTS.md'), 'fixture\n')
    // bake-rs no longer reads CLAUDE.md, nor an AGENTS.md beside the user's .agents.
    writeFileSync(join(dir, 'a/CLAUDE.md'), 'ignored\n')
    writeFileSync(join(userHome, 'AGENTS.md'), 'ignored\n')
    const owned = strayContextFiles(workspace, userHome, true)
    expect(owned).toContain(join(userHome, '.agents/AGENTS.md'))
    expect(owned).toContain(join(realpathSync(dir), 'a/AGENTS.md'))
    expect(owned).not.toContain(join(realpathSync(workspace), 'AGENTS.md'))
    expect(owned).not.toContain(join(realpathSync(dir), 'a/CLAUDE.md'))
    expect(owned).not.toContain(join(userHome, 'AGENTS.md'))
    expect(strayContextFiles(workspace, userHome, false)).toContain(join(realpathSync(workspace), 'AGENTS.md'))
  })

  test('names skill directories in ~/.agents and up to the git root', () => {
    const dir = mkdtempSync(join(root, 'skills-'))
    const workspace = join(dir, 'repo/workspace')
    const userHome = join(dir, 'user')
    mkdirSync(join(userHome, '.agents/skills'), { recursive: true })
    mkdirSync(join(workspace, '.agents/skills'), { recursive: true })
    mkdirSync(join(dir, 'repo/.agents/skills'), { recursive: true })
    mkdirSync(join(dir, '.agents/skills'), { recursive: true })
    const real = realpathSync(dir)
    const outside = strayContextFiles(workspace, userHome, false)
    expect(outside).toContain(join(userHome, '.agents/skills'))
    expect(outside).toContain(join(real, 'repo/workspace/.agents/skills'))
    expect(outside).toContain(join(real, 'repo/.agents/skills'))
    expect(outside).toContain(join(real, '.agents/skills'))
    // A repository root ends the skill search; context files are still read above it.
    mkdirSync(join(dir, 'repo/.git'))
    const inside = strayContextFiles(workspace, userHome, false)
    expect(inside).toContain(join(real, 'repo/.agents/skills'))
    expect(inside).not.toContain(join(real, '.agents/skills'))
    // A skill scenario owns the workspace's skills, but not those of its ancestors or the user.
    const owned = strayContextFiles(workspace, userHome, false, true)
    expect(owned).not.toContain(join(real, 'repo/workspace/.agents/skills'))
    expect(owned).toContain(join(real, 'repo/.agents/skills'))
    expect(owned).toContain(join(userHome, '.agents/skills'))
  })
})
