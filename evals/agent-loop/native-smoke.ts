/**
 * Runs the built Rust test arm through the fixture-only eval adapter. Correct
 * effects must pass; a passing rewritten check, empty success claim, or failed
 * process must not. This measures no model usage or native agent behavior.
 */
import { deepStrictEqual } from 'node:assert'
import { createHash } from 'node:crypto'
import { readFileSync } from 'node:fs'
import { join, resolve } from 'node:path'
import { runNativeFixture } from './native-fixture.ts'
import { prompts } from './scenarios.ts'

const root = resolve(import.meta.dir, '../..')
const binary = join(root, 'rust/target/debug', `bake-eval-fake-arm${process.platform === 'win32' ? '.exe' : ''}`)
const sha256 = (bytes: string | Uint8Array): string => createHash('sha256').update(bytes).digest('hex')
const modes = ['edit', 'tamper-tests', 'claim-only', 'edit-exit1'] as const

/** Exercise the real compiled arm; throws if an independent observation disagrees. */
async function smokeNativeFixture(): Promise<void> {
  const digest = sha256(readFileSync(binary))
  const stop = new AbortController()
  const onSignal = (): void => stop.abort()
  process.once('SIGINT', onSignal)
  process.once('SIGTERM', onSignal)
  try {
    for (const mode of modes) {
      const run = await runNativeFixture({ argv: [binary, mode], scenario: 'ordinary_edit', timeoutMs: 30_000, signal: stop.signal })
      if (stop.signal.aborted) throw new Error('native eval smoke cancelled')
      try {
        const observation: unknown = JSON.parse(run.stdout.split('\n')[0] ?? '')
        deepStrictEqual(observation, { mode, prompt: prompts.ordinary_edit }, `${mode}: compiled arm prompt and mode`)
        if (run.promptSha256 !== sha256(prompts.ordinary_edit!)) throw new Error(`${mode}: prompt digest mismatch`)
        const checksPass = mode !== 'claim-only'
        const testsUnchanged = mode !== 'tamper-tests'
        if (run.success !== (mode === 'edit') || run.verdict.validated !== (checksPass && testsUnchanged)
          || run.verdict.testsUnchanged !== testsUnchanged || (run.verdict.testExit === 0) !== checksPass
          || run.process.exitCode !== (mode === 'edit-exit1' ? 1 : 0) || run.process.signal !== null
          || run.process.timedOut || run.process.cancelled || run.process.stdoutOverflow || run.process.stderrOverflow
          || run.process.spawnError !== undefined || run.process.stdinError !== undefined || run.process.stopError !== undefined) {
          throw new Error(`${mode}: unexpected fixture or process verdict: ${JSON.stringify({ process: run.process, verdict: run.verdict, success: run.success })}`)
        }
      } catch (error) {
        throw new Error(`${mode}: ${(error as Error).message}; process=${JSON.stringify(run.process)}; stderr=${JSON.stringify(run.stderr)}`, { cause: error })
      }
      console.log(`pass native eval fixture ${mode}: ${run.success ? 'accepted' : 'rejected'}`)
    }
    console.log(JSON.stringify({ schema: 'bake/native-eval-smoke', version: 1, platform: process.platform, arch: process.arch,
      binarySha256: digest, promptSha256: sha256(prompts.ordinary_edit!), cases: modes.length, passed: true }))
  } finally {
    process.removeListener('SIGINT', onSignal)
    process.removeListener('SIGTERM', onSignal)
  }
}

if (import.meta.main) {
  try { await smokeNativeFixture() } catch (error) {
    console.error(`native eval smoke failed: ${(error as Error).message}`)
    process.exitCode = 1
  }
}
