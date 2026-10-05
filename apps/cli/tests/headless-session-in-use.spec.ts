/**
 * `dsh --profile headless --resume <id>` through a real launcher process while
 * another process holds the session's kernel write lock: the refusal is the
 * terminal profile's launch line on stderr, stdout stays empty, the process
 * exits with the in-use status, and the held log is left as it was.
 */

import { readdirSync, readFileSync, realpathSync } from 'node:fs'
import { mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { execa } from 'execa'
import { describe, expect, it, onTestFinished } from 'vitest'
import { Context } from '@deepseek-ai/cordis'
import { SESSION_IN_USE_EXIT } from 'bake-cmdline'
import { LOADER_SMOKE_TEST_TIMEOUT_MS, resolveExampleLaunch } from 'bake-loader-smoke'
import { SESSION_FORMAT_VERSION, SessionId, SessionSeq } from 'bake-session'
import Persistence from 'bake-session-persistence-jsonl'
import { dictionaries } from 'bake-tui-ui/copy.ts'
import { deepseekEndpointSettings } from './fixtures/deepseek-endpoint.ts'

const dshBinScript = fileURLToPath(new URL('../src/bin.ts', import.meta.url))
const tsconfigPath = fileURLToPath(new URL('../../../tsconfig.json', import.meta.url))

describe('headless --resume of a session another process has open (real launcher)', () => {
  it('prints the terminal profile\'s refusal as one stderr line and exits 75 without writing', async () => {
    const cwd = realpathSync(await mkdtemp(join(tmpdir(), 'dsh-in-use-')))
    onTestFinished(() => rm(cwd, { recursive: true, force: true, maxRetries: 3 }))
    const sessions = join(cwd, '.dsh', 'sessions')
    const id = SessionId('session-held')

    // Another Bake process: its own store over the profile's session root,
    // holding the write lock of an adoptable session recorded in this workspace.
    const other = new Context()
    onTestFinished(() => other.fiber.dispose())
    await other.plugin(Persistence, { root: sessions, compression: 'none' })
    const handle = await other.sessionPersistence.create({
      version: SESSION_FORMAT_VERSION, id, createdAt: Date.now(), cwd, isSeeded: false,
    })
    onTestFinished(() => handle.close())
    await handle.append([
      { type: 'turn/start', seq: SessionSeq(0), time: 1, data: { turn: 1 } },
      { type: 'turn/end', seq: SessionSeq(1), time: 2, data: { turn: 1, reason: { kind: 'completed' } } },
    ])
    await handle.flush()
    const logs = (): Record<string, string> => Object.fromEntries(
      readdirSync(sessions, { recursive: true, encoding: 'utf8' }).filter(path => path.endsWith('.jsonl'))
        .map(path => [path, readFileSync(join(sessions, path), 'utf8')]),
    )
    const before = logs()
    expect(Object.keys(before)).toHaveLength(1)

    // The refusal comes before any model request; a request would fail fast here.
    await writeFile(join(cwd, '.dsh', 'settings.yaml'), deepseekEndpointSettings('http://127.0.0.1:9'))
    const patch = join(cwd, 'in-use.patch.yml')
    await writeFile(patch, [
      '- id: session-persistence-jsonl',
      '  config:',
      "    root: !!js dshHomePath('sessions')",
      '    compression: none',
      '',
    ].join('\n'))
    const launch = resolveExampleLaunch({
      srcBin: dshBinScript,
      configArgs: ['--profile', 'headless', '--patch', patch, '--resume', id, 'continue'],
      tsconfigPath,
      env: {
        DSH_HOME: join(cwd, '.dsh'),
        DSH_AGENTS_HOME: join(cwd, '.agents'),
        DSH_TELEMETRY_DISABLED: '1',
        DEEPSEEK_API_KEY: 'keyless-in-use',
      },
    })
    const outcome = await execa(launch.command, launch.args, {
      cwd,
      env: launch.env,
      stdin: 'ignore',
      reject: false,
      stripFinalNewline: false,
      timeout: LOADER_SMOKE_TEST_TIMEOUT_MS - 5_000,
      killSignal: 'SIGKILL',
    })

    expect(outcome.stderr).toBe(`dsh: ${id}: ${dictionaries.en.sessionInUseLaunch}\n`)
    expect(outcome.stdout).toBe('')
    expect(outcome.exitCode).toBe(SESSION_IN_USE_EXIT)
    expect(logs()).toEqual(before)
  }, LOADER_SMOKE_TEST_TIMEOUT_MS)
})
