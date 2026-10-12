/** Native arm resolution and launch arguments, without Cargo or a model. */
import { afterAll, describe, expect, test } from 'bun:test'
import { createHash } from 'node:crypto'
import { chmodSync, mkdirSync, mkdtempSync, realpathSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { nativeCommand, parseArms } from './arms.ts'

const root = mkdtempSync(join(tmpdir(), 'bake-eval-arms-'))
afterAll(() => rmSync(root, { recursive: true, force: true }))

describe('native arms', () => {
  test('launch print mode with JSON events, no session, the full-access preset, and the prompt after --', () => {
    expect(nativeCommand('/bin/bake-rs', 'cliproxyapi', 'gpt-x', 'medium', '-starts with a dash'))
      .toEqual(['/bin/bake-rs', '-p', '--mode', 'json', '--no-session', '--permission', 'danger-full-access',
        '--model', 'cliproxyapi/gpt-x', '--thinking', 'medium', '--', '-starts with a dash'])
  })

  // The stand-in binary is a shell script, so this case needs a POSIX shell.
  test.skipIf(process.platform === 'win32')('a binary path is run as given and recorded with its version and digest', () => {
    const checkout = join(root, 'checkout')
    mkdirSync(join(checkout, 'rust/target/release'), { recursive: true })
    writeFileSync(join(checkout, 'rust/Cargo.toml'), '[workspace]\n')
    const bin = join(checkout, 'rust/target/release/bake-rs')
    const script = '#!/bin/sh\necho "bake-rs 9.9.9-test"\n'
    writeFileSync(bin, script)
    chmodSync(bin, 0o755)
    const arm = parseArms(`cand=native:${bin}`, 'headless', {}).cand!
    expect(arm).toEqual({
      kind: 'native', root: realpathSync(checkout), bin: realpathSync(bin), version: 'bake-rs 9.9.9-test',
      binarySha256: createHash('sha256').update(script).digest('hex'),
      extra: [], settings: '{}', llmDeepseek: false, composition: [],
    })
  })

  test('a missing path or a directory without a Rust workspace fails before any sample', () => {
    expect(() => parseArms(`cand=native:${join(root, 'missing')}`, 'headless', {})).toThrow(/does not exist/)
    mkdirSync(join(root, 'plain'))
    expect(() => parseArms(`cand=native:${join(root, 'plain')}`, 'headless', {})).toThrow(/no rust\/Cargo.toml/)
  })
})
