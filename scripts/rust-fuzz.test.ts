import { describe, expect, it } from 'bun:test'
import { readFileSync } from 'node:fs'
import { join } from 'node:path'
import { crashArtifacts, crashReport, DEFAULT_SECONDS, FUZZ_DIR, fuzzCommand, fuzzTargets, lastStats, parseFuzzArgs } from './rust-fuzz.ts'

const KNOWN = ['session_jsonl', 'tui_text']

describe('Rust fuzz runner', () => {
  it('lists the committed fuzz crate targets, each with a seed corpus', () => {
    const targets = fuzzTargets(readFileSync(join(FUZZ_DIR, 'Cargo.toml'), 'utf8'))
    expect(targets.length).toBeGreaterThanOrEqual(4)
    for (const target of targets) {
      const seeds = new Bun.Glob('seed-*').scanSync(join(FUZZ_DIR, 'corpus', target))
      expect([...seeds].length).toBeGreaterThan(0)
    }
  })

  it('reads the bin names of a manifest in order', () => {
    const manifest = '[package]\nname = "x"\n\n[[bin]]\nname = "b"\npath = "b.rs"\n\n[[bin]]\nname = "a"\npath = "a.rs"\n'
    expect(fuzzTargets(manifest)).toEqual(['b', 'a'])
    expect(fuzzTargets('[package]\nname = "x"\n')).toEqual([])
  })

  it('parses list, one target, and all targets with their limits', () => {
    expect(parseFuzzArgs(['--list'], KNOWN)).toEqual({ kind: 'list' })
    expect(parseFuzzArgs(['tui_text'], KNOWN)).toEqual({ kind: 'run', targets: ['tui_text'], seconds: DEFAULT_SECONDS, jobs: 1 })
    expect(parseFuzzArgs(['tui_text', '--seconds', '120', '--jobs', '4'], KNOWN))
      .toEqual({ kind: 'run', targets: ['tui_text'], seconds: 120, jobs: 4 })
    expect(parseFuzzArgs(['--all', '--seconds', '5'], KNOWN)).toEqual({ kind: 'run', targets: KNOWN, seconds: 5, jobs: 1 })
  })

  it('refuses unknown targets, missing or bad numbers, and mixed modes', () => {
    for (const args of [
      [], ['nope'], ['tui_text', 'session_jsonl'], ['--all', 'tui_text'], ['--list', 'tui_text'], ['--list', '--all'],
      ['tui_text', '--seconds'], ['tui_text', '--seconds', '0'], ['tui_text', '--jobs', '-1'], ['tui_text', '--seconds', '1.5'],
      ['tui_text', '--runs', '3'],
    ]) {
      expect(() => parseFuzzArgs(args, KNOWN)).toThrow()
    }
  })

  it('runs the target over its grown corpus first, then its seeds, for a bounded time', () => {
    expect(fuzzCommand('tui_text', 120, 1)).toEqual([
      'cargo', 'fuzz', 'run', 'tui_text', 'generated/tui_text', 'corpus/tui_text', '--', '-max_total_time=120', '-print_final_stats=1',
    ])
  })

  it('passes parallel jobs to cargo-fuzz, which forks libFuzzer workers', () => {
    const command = fuzzCommand('session_jsonl', 30, 8)
    expect(command.slice(0, 6)).toEqual(['cargo', 'fuzz', 'run', 'session_jsonl', '--jobs', '8'])
    expect(command.slice(command.indexOf('--') + 1)).toEqual(['-max_total_time=30', '-print_final_stats=1'])
  })

  it('finds crash inputs and the last coverage line in libFuzzer output', () => {
    const output = [
      '#2\tINITED cov: 10 ft: 11 corp: 1/1b exec/s: 0 rss: 30Mb',
      '#4096\tpulse  cov: 120 ft: 300 corp: 9/90b lim: 4 exec/s: 2048 rss: 40Mb\r#8192\tpulse  cov: 130 ft: 333 corp: 10/99b lim: 4 exec/s: 4096 rss: 41Mb',
      "artifact_prefix='/r/rust/fuzz/artifacts/tui_text/'; Test unit written to /r/rust/fuzz/artifacts/tui_text/crash-abc",
      'Test unit written to ./slow-unit-def',
    ].join('\n')
    expect(crashArtifacts(output)).toEqual(['/r/rust/fuzz/artifacts/tui_text/crash-abc', './slow-unit-def'])
    expect(lastStats(output)).toEqual({ cov: 130, ft: 333, execsPerSecond: 4096 })
    expect(lastStats('no stats here')).toBeUndefined()
    expect(crashArtifacts('Done 10 runs in 1 second(s)')).toEqual([])
  })

  it('takes the merged totals of fork mode over the lines of its workers', () => {
    const output = [
      '#9000\tpulse  cov: 50 ft: 60 corp: 3/9b lim: 4 exec/s: 900 rss: 40Mb',
      '#75546: cov: 3261 ft: 17821 corp: 2689 exec/s: 2485 oom/timeout/crash: 0/0/0 time: 16s job: 10 dft_time: 0',
      '#4961\tREDUCE cov: 2752 ft: 13694 corp: 960/109Kb lim: 191 exec/s: 2480 rss: 419Mb L: 73/191 MS: 1 EraseBytes-',
    ].join('\n')
    expect(lastStats(output)).toEqual({ cov: 3261, ft: 17821, execsPerSecond: 2485 })
  })

  it('reports a crash with the commands that reproduce and minimize it', () => {
    const report = crashReport('tui_text', ['/r/rust/fuzz/artifacts/tui_text/crash-abc'], '/r')
    expect(report).toContain('rust/fuzz/artifacts/tui_text/crash-abc')
    expect(report).toContain('cd rust/fuzz && cargo fuzz run tui_text artifacts/tui_text/crash-abc')
    expect(report).toContain('cd rust/fuzz && cargo fuzz tmin tui_text artifacts/tui_text/crash-abc')
  })
})
