/**
 * REAL-composition tier for the shipped telemetry row: the headless profile
 * over bake-base, booted through the Loader in a child process with a local
 * OTLP collector. Session upload has no default destination; only
 * `DSH_TELEMETRY_OTLP_URL` turns it on, and `DSH_TELEMETRY_DISABLED` still
 * turns it off. `/feedback` tells the user which of these applies.
 */

import { readFile, readdir } from 'node:fs/promises'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { describe, expect, it } from 'vitest'
import { LOADER_SMOKE_TEST_TIMEOUT_MS, runLoaderSmoke } from 'bake-loader-smoke'
import { sharingNotice } from 'bake-command-feedback'

const driver = fileURLToPath(new URL('./fixtures/driver.ts', import.meta.url))
const configPath = fileURLToPath(new URL('./fixtures/telemetry.patch.yml', import.meta.url))
const repoTsconfig = fileURLToPath(new URL('../../../../tsconfig.json', import.meta.url))

const LOCAL = 'Telemetry is off; this feedback stays in the local session log.'
const UPLOADED = 'This session’s history up to now is uploaded with this feedback to the configured telemetry collector. '
  + 'Set DSH_TELEMETRY_DISABLED=1 to keep feedback local.'

interface TelemetryRun {
  collectorUrl: string
  sharing?: string
  acknowledgement?: string
  outbound: string[]
}

interface Observed {
  run: TelemetryRun
  wire: string
  captures: unknown[]
  logContent: string
}

async function jsonlFiles(dir: string): Promise<string[]> {
  const entries = await readdir(dir, { withFileTypes: true })
  const paths = await Promise.all(entries.map(async (entry) => {
    const path = join(dir, entry.name)
    if (entry.isDirectory()) return jsonlFiles(path)
    return entry.isFile() && entry.name.endsWith('.jsonl') ? [path] : []
  }))
  return paths.flat()
}

/** Run the driver once with the given launch environment and collect what it observed. */
async function observe(label: string, env: NodeJS.ProcessEnv): Promise<Observed> {
  let observed!: Observed
  const { stderr } = await runLoaderSmoke({
    label: `session-telemetry-otel ${label}`,
    tempDirPrefix: 'telemetry-otel-base-',
    binScript: driver,
    libBinScript: driver,
    configPath,
    tsconfigPath: repoTsconfig,
    // The developer's own telemetry variables must not leak into the child.
    env: { DSH_TELEMETRY_OTLP_URL: undefined, DSH_TELEMETRY_MODE: undefined, DSH_TELEMETRY_DISABLED: undefined, ...env },
    inspect: async (cwd) => {
      const captures = JSON.parse(await readFile(join(cwd, 'otlp-captures.json'), 'utf8')) as unknown[]
      const logs = await jsonlFiles(join(cwd, '.sessions'))
      expect(logs).toHaveLength(1)
      observed = {
        run: JSON.parse(await readFile(join(cwd, 'telemetry-run.json'), 'utf8')) as TelemetryRun,
        wire: JSON.stringify(captures),
        captures,
        logContent: await readFile(logs[0] as string, 'utf8'),
      }
    },
  })
  expect(stderr).not.toContain('UNHANDLED')
  return observed
}

function sharingLine(run: TelemetryRun): string | undefined {
  return run.acknowledgement?.split('\n').at(-1)
}

describe('shipped session telemetry endpoint', () => {
  it('uploads nothing by default and tells /feedback the history stays local', async () => {
    const { run, captures, logContent } = await observe('default endpoint', { DSH_TELEMETRY_E2E_ENDPOINT: 'unset' })

    expect(run.sharing).toBe('disabled')
    expect(sharingLine(run)).toBe(LOCAL)
    expect(captures).toEqual([])
    // No HTTP request of any kind left the process, so no collector could receive one.
    expect(run.outbound).toEqual([])
    expect(logContent).toContain('fixture feedback')
    expect(logContent).toContain('sk-e2efixture1234567890')
  }, LOADER_SMOKE_TEST_TIMEOUT_MS)

  it('uploads the feedback-authorized prefix only to DSH_TELEMETRY_OTLP_URL', async () => {
    const { run, captures, wire } = await observe('configured endpoint', {})

    expect(run.sharing).toBe('feedback-only')
    expect(sharingLine(run)).toBe(UPLOADED)
    expect(sharingLine(run)).toBe(sharingNotice('feedback-only'))
    expect(captures.length).toBeGreaterThan(0)
    expect(run.outbound.length).toBeGreaterThan(0)
    const collector = new URL(run.collectorUrl)
    for (const request of run.outbound) expect(request).toBe(`POST http://${collector.host}${collector.pathname}`)
    expect(wire).toContain('prove telemetry with key')
    expect(wire).toContain('fixture feedback')
    expect(wire).not.toContain('sk-e2efixture1234567890')
    expect(wire).not.toContain('post-feedback private suffix')
  }, LOADER_SMOKE_TEST_TIMEOUT_MS)

  it('keeps a configured endpoint silent under DSH_TELEMETRY_DISABLED', async () => {
    const { run, captures } = await observe('switched-off endpoint', { DSH_TELEMETRY_DISABLED: '1' })

    // The switch unmounts the backend, so no sharing mode is reported at all.
    expect(run.sharing).toBeUndefined()
    expect(sharingLine(run)).toBe(LOCAL)
    expect(captures).toEqual([])
    expect(run.outbound).toEqual([])
  }, LOADER_SMOKE_TEST_TIMEOUT_MS)
})
