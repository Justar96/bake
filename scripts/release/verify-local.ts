#!/usr/bin/env bun
/** Exercise the actual download server and installer against a temporary home. */

import { existsSync, mkdtempSync, readFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { windowsPowerShellEnvironment } from './powershell.ts'

const ROOT = resolve(import.meta.dir, '../..')
const temporary = mkdtempSync(join(tmpdir(), 'bake-release-verify-'))
const server = Bun.spawn(['node', 'distribution/host/server.mjs'], {
  cwd: ROOT, env: { ...process.env, PORT: '0' }, stdout: 'pipe', stderr: 'inherit',
})

async function run(argv: string[], env: NodeJS.ProcessEnv, cwd = ROOT): Promise<string> {
  const child = Bun.spawn(argv, { cwd, env, stdout: 'pipe', stderr: 'pipe' })
  const [stdout, stderr, code] = await Promise.all([
    new Response(child.stdout).text(), new Response(child.stderr).text(), child.exited,
  ])
  if (code !== 0) throw new Error(`${argv.join(' ')} exited ${code}: ${stderr}`)
  return stdout
}

try {
  const reader = server.stdout.getReader()
  const timer = setTimeout(() => server.kill(), 10_000)
  let output = ''
  while (!output.includes('\n')) {
    const next = await reader.read()
    if (next.done) throw new Error('Download server exited before listening')
    output += new TextDecoder().decode(next.value)
  }
  clearTimeout(timer)
  reader.releaseLock()
  const port = /^Bake downloads listening on (\d+)$/m.exec(output)?.[1]
  if (port === undefined) throw new Error(`Unexpected download server output: ${output}`)
  const base = `http://127.0.0.1:${port}`
  const health = await fetch(`${base}/health`)
  if (health.status !== 200) throw new Error('Download server health check failed')
  const page = await fetch(base)
  if (page.status !== 200 || !(await page.text()).includes('/install.ps1')) {
    throw new Error('Download page did not serve the installer commands')
  }
  const manifest = await (await fetch(`${base}/latest.json`)).json() as {
    version: string
    artifacts: Record<string, { sha256: string }>
  }
  const installRoot = join(temporary, 'install')
  const binDir = join(temporary, 'bin')
  const env = {
    ...process.env, BAKE_RELEASE_BASE_URL: base, BAKE_INSTALL_ROOT: installRoot,
    BAKE_BIN_DIR: binDir, BAKE_SKIP_PATH_UPDATE: '1', DSH_HOME: join(temporary, 'home'),
  }
  if (process.platform === 'win32') {
    const install = ['powershell.exe', '-NoProfile', '-ExecutionPolicy', 'Bypass', '-Command',
      'Invoke-RestMethod "$env:BAKE_RELEASE_BASE_URL/install.ps1" | Invoke-Expression']
    const installEnv = windowsPowerShellEnvironment(env)
    await run(install, installEnv)
    await run(install, installEnv)
    const version = await run(['cmd.exe', '/c', join(binDir, 'bake.cmd'), '--version'], env)
    if (!version.includes(manifest.version)) throw new Error('Installed Windows command version mismatch')
    const config = await run(['cmd.exe', '/c', join(binDir, 'bake.cmd'), 'tui', '--dump-default-config'], env)
    if (!config.includes('@deepseek-ai/dsh-base')) throw new Error('Windows profile routing failed')
    const defaultHome: NodeJS.ProcessEnv = { ...env, USERPROFILE: temporary }
    delete defaultHome.DSH_HOME
    await run(['cmd.exe', '/c', join(binDir, 'bake.cmd'), '--help'], defaultHome)
  } else {
    const install = ['sh', '-c', 'curl -fsSL "$BAKE_RELEASE_BASE_URL/install.sh" | sh']
    await run(install, env)
    await run(install, env)
    const version = await run([join(binDir, 'bake'), '--version'], env)
    if (!version.includes(manifest.version)) throw new Error('Installed command version mismatch')
    const config = await run([join(binDir, 'bake'), 'tui', '--dump-default-config'], env)
    if (!config.includes('@deepseek-ai/dsh-base')) throw new Error('Installed profile routing failed')
    const help = await run([join(binDir, 'bake'), '--help'], env)
    if (!help.includes('Usage:')) throw new Error('Installed command did not boot the terminal profile')
    const defaultHome: NodeJS.ProcessEnv = { ...env, HOME: temporary }
    delete defaultHome.DSH_HOME
    await run([join(binDir, 'bake'), '--help'], defaultHome)
  }
  if (!existsSync(join(temporary, '.bake/profiles/tui/package.json'))) {
    throw new Error('Installed command did not use Bake as its default home')
  }
  const target = `${process.platform}-${process.arch}`
  const artifact = manifest.artifacts[target]
  if (artifact === undefined) throw new Error(`No release artifact for ${target}`)
  const installed = join(installRoot, 'versions', `${manifest.version}-${artifact.sha256.slice(0, 12)}`)
  const installedVersion = (JSON.parse(readFileSync(join(installed, 'package.json'), 'utf8')) as { version: string }).version
  if (installedVersion !== manifest.version) throw new Error('Installed Bake root version mismatch')
  if (!readFileSync(join(installed, 'CHANGELOG.md'), 'utf8').includes(`## [${manifest.version}]`)) {
    throw new Error('Installed changelog has no entry for this release')
  }
  const native = 'const p = require(\'node-pty\'); const child = p.spawn(process.execPath, [\'-e\', \'process.stdout.write("NATIVE_OK")\'], { name: \'xterm-color\', cols: 80, rows: 24, cwd: process.cwd(), env: process.env }); let output = \'\'; child.onData(data => output += data); child.onExit(({exitCode}) => process.exit(exitCode === 0 && output.includes(\'NATIVE_OK\') ? 0 : 1));'
  await run(['node', '-e', native], env, join(installed, 'packages/subprocess/subprocess-local'))
  const staged = JSON.parse(readFileSync(join(ROOT, 'distribution/host/public/latest.json'), 'utf8')) as { version: string }
  if (staged.version !== manifest.version) throw new Error('Served a different release manifest')
  console.log(`Verified local Bake ${manifest.version} install from ${base}`)
} finally {
  server.kill()
  await server.exited
  rmSync(temporary, { recursive: true, force: true })
}
