import { describe, expect, it } from 'bun:test'
import { parseJunit, parsePreflightSummary, renderStepSummary } from './ci-step-summary.ts'

// The shape cargo-nextest 0.9.148 writes with `report-skipped = "all"` (rust/.config/nextest.toml).
const JUNIT = `<?xml version="1.0" encoding="UTF-8"?>
<testsuites name="nextest-run" tests="5" skipped="1" failures="1" errors="0" time="7.100">
    <testsuite name="bake-tui-view" tests="3" skipped="1" errors="0" failures="0">
        <testcase name="activity::tests::skipped_case" classname="bake-tui-view" time="0.000">
            <skipped message="Skipped: test does not match the provided expression filters"/>
        </testcase>
        <testcase name="render::tests::fast" classname="bake-tui-view" timestamp="2026-10-21T02:40:51.688+07:00" time="0.003"/>
        <testcase name="render::tests::a_&lt;pipe|name&gt;" classname="bake-tui-view" timestamp="2026-10-21T02:40:51.688+07:00" time="0.069"/>
    </testsuite>
    <testsuite name="bake-cli::print_mode" tests="2" skipped="0" errors="0" failures="1">
        <testcase name="slow_case" classname="bake-cli::print_mode" timestamp="2026-10-21T02:40:51.688+07:00" time="6.500"/>
        <testcase name="failing_case" classname="bake-cli::print_mode" timestamp="2026-10-21T02:40:51.688+07:00" time="0.528">
            <failure message="test failed with exit code 101" type="test failure with exit code 101">thread panicked</failure>
            <system-out>running 1 test</system-out>
        </testcase>
    </testsuite>
</testsuites>
`

describe('CI step summary', () => {
  it('reads the cases that ran, with their binary, time, and failure', () => {
    expect(parseJunit(JUNIT)).toEqual([
      { name: 'render::tests::fast', suite: 'bake-tui-view', seconds: 0.003, failed: false },
      { name: 'render::tests::a_<pipe|name>', suite: 'bake-tui-view', seconds: 0.069, failed: false },
      { name: 'slow_case', suite: 'bake-cli::print_mode', seconds: 6.5, failed: false },
      { name: 'failing_case', suite: 'bake-cli::print_mode', seconds: 0.528, failed: true },
    ])
    expect(parseJunit('<testsuites/>')).toEqual([])
  })

  it('reads preflight steps and drops malformed entries', () => {
    expect(parsePreflightSummary(JSON.stringify({ seconds: 3, steps: [
      { step: 'rust-fmt', outcome: 'pass', seconds: 1.2 },
      { step: 'rust-doc', outcome: 'skip', seconds: 0, note: 'no Rust input changed' },
      { step: 'broken', outcome: 'pass' },
      null,
    ] }))).toEqual([
      { step: 'rust-fmt', outcome: 'pass', seconds: 1.2 },
      { step: 'rust-doc', outcome: 'skip', seconds: 0 },
    ])
    expect(parsePreflightSummary('[]')).toEqual([])
  })

  it('renders step times and the slowest tests first, escaping table cells', () => {
    const markdown = renderStepSummary('rust (ubuntu-latest)', [
      { step: 'rust-fmt', outcome: 'pass', seconds: 1.25 },
      { step: 'rust-test', outcome: 'fail', seconds: 40 },
      { step: 'rust-pty', outcome: 'skip', seconds: 0 },
    ], parseJunit(JUNIT))
    expect(markdown).toBe(`### rust (ubuntu-latest) timings

| Step | Outcome | Seconds |
| --- | --- | ---: |
| rust-fmt | pass | 1.3 |
| rust-test | fail | 40.0 |
| rust-pty | skip |  |
| total of steps run | | 41.3 |

<details><summary>4 slowest of 4 nextest tests (7 test-seconds)</summary>

| Test | Binary | Seconds |
| --- | --- | ---: |
| slow_case | bake-cli::print_mode | 6.50 |
| failing_case (failed) | bake-cli::print_mode | 0.53 |
| render::tests::a_&lt;pipe\\|name&gt; | bake-tui-view | 0.07 |
| render::tests::fast | bake-tui-view | 0.00 |

</details>

`)
  })

  it('keeps only the twenty slowest and notes a missing preflight summary', () => {
    const tests = Array.from({ length: 25 }, (_, index) => ({ name: `t${index}`, suite: 's', seconds: index, failed: false }))
    const markdown = renderStepSummary('checks', undefined, tests)
    expect(markdown).toContain('No preflight summary was written.')
    expect(markdown).toContain('20 slowest of 25 nextest tests')
    expect(markdown).toContain('| t24 | s | 24.00 |')
    expect(markdown).toContain('| t5 | s | 5.00 |')
    expect(markdown).not.toContain('| t4 |')
    expect(renderStepSummary('checks', [], undefined)).not.toContain('slowest')
  })
})
