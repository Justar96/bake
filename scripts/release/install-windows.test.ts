/** Windows PowerShell must accept Node 24+ before the installer downloads a release. */
import { afterEach, describe, expect, test } from 'bun:test'
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { windowsPowerShellEnvironment } from './powershell.ts'

const roots: string[] = []
afterEach(() => { for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true }) })

/** Mock the download boundary while exercising the actual PowerShell installer preflight. */
async function preflight(setup = ''): Promise<{ code: number; stdout: string; stderr: string }> {
  const root = mkdtempSync(join(tmpdir(), 'bake-windows-installer-test-'))
  roots.push(root)
  const driver = join(root, 'preflight.ps1')
  writeFileSync(driver, `${setup}
function Invoke-WebRequest { Write-Output 'MANIFEST_REACHED'; exit 0 }
& $args[0]
`)
  const child = Bun.spawn(['powershell.exe', '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', driver,
    resolve(import.meta.dir, '../../distribution/host/install.ps1')], {
    env: windowsPowerShellEnvironment(process.env), stdout: 'pipe', stderr: 'pipe', timeout: 10_000,
  })
  const [code, stdout, stderr] = await Promise.all([
    child.exited, new Response(child.stdout).text(), new Response(child.stderr).text(),
  ])
  return { code, stdout, stderr }
}

describe.skipIf(process.platform !== 'win32')('Windows installer Node preflight', () => {
  test('accepts the installed supported Node without losing native argument quotes', async () => {
    const result = await preflight()
    expect(result.stderr).toBe('')
    expect(result.code).toBe(0)
    expect(result.stdout.trim()).toBe('MANIFEST_REACHED')
  })

  test('loads Windows PowerShell utility functions with inherited Core module paths', async () => {
    const result = await preflight('Get-Command Get-FileHash -ErrorAction Stop | Out-Null')
    expect(result.stderr).toBe('')
    expect(result.code).toBe(0)
    expect(result.stdout.trim()).toBe('MANIFEST_REACHED')
  })

  test.each(['22.19.0', '23.0.0'])('rejects unsupported Node %s before downloading', async (version) => {
    const result = await preflight(`function node { '${version}' }`)
    expect(result.code).not.toBe(0)
    expect(result.stderr).toContain('Bake install needs Node.js 24 or newer on PATH.')
    expect(result.stdout).not.toContain('MANIFEST_REACHED')
  })

  test('reports a missing Node executable before downloading', async () => {
    const result = await preflight("function node { throw 'Node unavailable' }")
    expect(result.code).not.toBe(0)
    expect(result.stderr).toContain('Bake install needs Node.js 24 or newer on PATH.')
    expect(result.stdout).not.toContain('MANIFEST_REACHED')
  })
})
