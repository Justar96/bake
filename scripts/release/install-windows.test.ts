/** Windows PowerShell must accept Node 24+ before the installer downloads a release. */
import { afterEach, describe, expect, test } from 'bun:test'
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { windowsPowerShellEnvironment } from './powershell.ts'
import { INSTALLER_DRAIN_MS, INSTALLER_WATCHDOG_MS, installerTestProcess } from './installer-test-process.ts'

const roots: string[] = []
const processes = new Set<ReturnType<typeof installerTestProcess>>()
const PREFLIGHT_TEST_TIMEOUT = 15_000
afterEach(async () => {
  const results = await Promise.all([...processes].map(child => child.stop('test-aborted')))
  processes.clear()
  for (const result of results) expect(result.cleanupErrors).toEqual([])
  for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true })
}, 2 * INSTALLER_DRAIN_MS + 5_000)

/** Mock the download boundary while exercising the actual PowerShell installer preflight. */
async function preflight(setup = ''): Promise<{ code: number; stdout: string; stderr: string }> {
  const root = mkdtempSync(join(tmpdir(), 'bake-windows-installer-test-'))
  roots.push(root)
  const driver = join(root, 'preflight.ps1')
  const phaseFile = join(root, 'phases.txt')
  const installer = resolve(import.meta.dir, '../../distribution/host/install.ps1')
  const lines = readFileSync(installer, 'utf8').split('\n')
  const beforeNode = lines.findIndex(line => line.startsWith('try { $nodeMajor =')) + 1
  const afterNode = lines.findIndex(line => line.startsWith('if ($nodeMajor -lt ')) + 1
  if (beforeNode === 0 || afterNode === 0) throw new Error('Installer Node preflight diagnostic anchors are missing')
  // Breakpoints observe the original script without wrapping node or changing its native arguments.
  writeFileSync(driver, `$global:BakePreflightPhaseFile = $args[1]
function global:Write-BakePreflightPhase([string]$Phase) {
  [IO.File]::AppendAllText($global:BakePreflightPhaseFile, ([DateTime]::UtcNow.ToString('o') + ' ' + $Phase + "\`n"))
}
Write-BakePreflightPhase 'powershell-entered'
${setup}
$null = Set-PSBreakpoint -Script $args[0] -Line ${beforeNode} -Action { Write-BakePreflightPhase 'before-node'; continue }
$null = Set-PSBreakpoint -Script $args[0] -Line ${afterNode} -Action { Write-BakePreflightPhase 'after-node'; continue }
Write-BakePreflightPhase 'installer-entered'
function Invoke-WebRequest { Write-BakePreflightPhase 'manifest-reached'; Write-Output 'MANIFEST_REACHED'; exit 0 }
& $args[0]
`)
  const child = installerTestProcess(['powershell.exe', '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', driver, installer, phaseFile], {
    env: windowsPowerShellEnvironment(process.env),
  })
  processes.add(child)
  const result = await child.done
  if (result.cleanupErrors.length === 0) processes.delete(child)
  const phases = existsSync(phaseFile) ? readFileSync(phaseFile, 'utf8').trim() : '(PowerShell entry was not observed)'
  if (result.timedOut || result.aborted || result.signal !== null || result.cleanupErrors.length > 0) {
    throw new Error(`Installer preflight: watchdog=${INSTALLER_WATCHDOG_MS} ms, elapsed=${result.elapsedMs} ms, timedOut=${result.timedOut}, aborted=${result.aborted}, signal=${result.signal}, exit=${result.code}\nPhases:\n${phases}\nCleanup: ${result.cleanupErrors.join('; ') || 'drained'}\nstdout:\n${result.stdout}\nstderr:\n${result.stderr}`)
  }
  // Keep successful-run timing in CI as well as the last completed phase when the watchdog fires.
  console.info(`Installer preflight (${result.elapsedMs} ms, exit ${result.code}):\n${phases}`)
  expect(phases).toContain('before-node')
  if (result.code === 0) expect(phases).toContain('after-node')
  return result
}

describe.skipIf(process.platform !== 'win32')('Windows installer Node preflight', () => {
  test('accepts the installed supported Node without losing native argument quotes', async () => {
    const result = await preflight()
    expect(result.stderr).toBe('')
    expect(result.code).toBe(0)
    expect(result.stdout.trim()).toBe('MANIFEST_REACHED')
  }, PREFLIGHT_TEST_TIMEOUT)

  test('loads Windows PowerShell utility functions with inherited Core module paths', async () => {
    const result = await preflight('Get-Command Get-FileHash -ErrorAction Stop | Out-Null')
    expect(result.stderr).toBe('')
    expect(result.code).toBe(0)
    expect(result.stdout.trim()).toBe('MANIFEST_REACHED')
  }, PREFLIGHT_TEST_TIMEOUT)

  test.each(['22.19.0', '23.0.0'])('rejects unsupported Node %s before downloading', async (version) => {
    const result = await preflight(`function node { '${version}' }`)
    expect(result.code).not.toBe(0)
    expect(result.stderr).toContain('Bake install needs Node.js 24 or newer on PATH.')
    expect(result.stdout).not.toContain('MANIFEST_REACHED')
  }, PREFLIGHT_TEST_TIMEOUT)

  test('reports a missing Node executable before downloading', async () => {
    const result = await preflight("function node { throw 'Node unavailable' }")
    expect(result.code).not.toBe(0)
    expect(result.stderr).toContain('Bake install needs Node.js 24 or newer on PATH.')
    expect(result.stdout).not.toContain('MANIFEST_REACHED')
  }, PREFLIGHT_TEST_TIMEOUT)
})
