#!/usr/bin/env bun
/**
 * Exercise the actual download server, installer, and updater against a
 * temporary home: install the staged release, then publish a newer one built
 * from it and update to that with `bake update`.
 */

import { createHash, generateKeyPairSync, sign } from 'node:crypto'
import { cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readlinkSync, rmSync, statSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { windowsPowerShellEnvironment } from './powershell.ts'

const ROOT = resolve(import.meta.dir, '../..')
const temporary = mkdtempSync(join(tmpdir(), 'bake-release-verify-'))
const stagedVersion = (JSON.parse(readFileSync(join(ROOT, 'apps/cli/package.json'), 'utf8')) as { version: string }).version
// A release assembled with --ephemeral-key leaves the key a client must be told to trust.
const ephemeral = join(ROOT, '.artifacts/bake-release', stagedVersion, 'ephemeral-release-key.pub')
const trusted = existsSync(ephemeral) ? readFileSync(ephemeral, 'utf8').trim() : undefined
const trust: NodeJS.ProcessEnv = trusted === undefined ? {} : { BAKE_RELEASE_PUBLIC_KEY: trusted }
const servers: ReturnType<typeof Bun.spawn>[] = []

async function run(argv: string[], env: NodeJS.ProcessEnv, cwd = ROOT, expected = 0): Promise<string> {
  const child = Bun.spawn(argv, { cwd, env, stdout: 'pipe', stderr: 'pipe' })
  const [stdout, stderr, code] = await Promise.all([
    new Response(child.stdout).text(), new Response(child.stderr).text(), child.exited,
  ])
  if (code !== expected) throw new Error(`${argv.join(' ')} exited ${code}, not ${expected}: ${stderr}`)
  return stdout
}

/** Start the download server over `host`'s files, and return its origin. */
async function serve(host: string): Promise<string> {
  const server = Bun.spawn(['node', join(host, 'server.mjs')], {
    cwd: host, env: { ...process.env, PORT: '0' }, stdout: 'pipe', stderr: 'inherit',
  })
  servers.push(server)
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
  return `http://127.0.0.1:${port}`
}

/**
 * Publish a release one patch newer than the staged `archive`, built from its
 * files, on a second host signed by a key of its own.
 * @returns the host origin, the newer version, and the key to trust for it.
 */
async function publishNewer(archive: string, target: string): Promise<{ base: string; version: string; key: string; directory: string }> {
  const [major = 0, minor = 0, patch = 0] = (stagedVersion.split('-')[0] ?? '').split('.').map(Number)
  const version = `${major}.${minor}.${patch + 1}`
  const tree = join(temporary, 'newer')
  const tar = process.platform === 'win32' ? 'tar.exe' : 'tar'
  // Unpacked, not copied from the install: Bun's cpSync recreates Windows
  // directory links as file links, which Node cannot resolve through.
  mkdirSync(tree)
  await run([tar, '-xzf', archive, '-C', tree], process.env)
  for (const manifest of ['package.json', 'apps/cli/package.json']) {
    const path = join(tree, manifest)
    writeFileSync(path, readFileSync(path, 'utf8').replace(`"version": "${stagedVersion}"`, `"version": "${version}"`))
  }
  const host = join(temporary, 'newer-host')
  mkdirSync(join(host, 'public/releases', version), { recursive: true })
  for (const file of ['server.mjs', 'index.html', 'install.sh', 'install.ps1']) cpSync(join(ROOT, 'distribution/host', file), join(host, file))
  const file = `bake-v${version}-${target}.tar.gz`
  const newer = join(host, 'public/releases', version, file)
  await run([tar, '-czf', newer, '-C', tree, '.'], process.env)
  const bytes = readFileSync(newer)
  const pair = generateKeyPairSync('ed25519')
  const manifest = Buffer.from(`${JSON.stringify({ version, artifacts: { [target]: {
    file, sha256: createHash('sha256').update(bytes).digest('hex'), size: statSync(newer).size,
  } } }, null, 2)}\n`)
  writeFileSync(join(host, 'public/latest.json'), manifest)
  writeFileSync(join(host, 'public/latest.json.sig'), `${sign(null, manifest, pair.privateKey).toString('base64')}\n`)
  const directory = `${version}-${createHash('sha256').update(bytes).digest('hex').slice(0, 12)}`
  return { base: await serve(host), version, key: pair.publicKey.export({ type: 'spki', format: 'der' }).toString('base64'), directory }
}

try {
  await run(['node', 'distribution/host/verify-manifest.mjs'], { ...process.env, ...trust })
  const base = await serve(join(ROOT, 'distribution/host'))
  const health = await fetch(`${base}/health`)
  if (health.status !== 200) throw new Error('Download server health check failed')
  const page = await fetch(base)
  if (page.status !== 200 || !(await page.text()).includes('/install.ps1')) {
    throw new Error('Download page did not serve the installer commands')
  }
  const manifest = await (await fetch(`${base}/latest.json`)).json() as {
    version: string
    artifacts: Record<string, { file: string; sha256: string }>
  }
  const installRoot = join(temporary, 'install')
  const binDir = join(temporary, 'bin')
  const env: NodeJS.ProcessEnv = {
    ...process.env, ...trust, BAKE_RELEASE_BASE_URL: base, BAKE_INSTALL_ROOT: installRoot,
    BAKE_BIN_DIR: binDir, BAKE_SKIP_PATH_UPDATE: '1', DSH_HOME: join(temporary, 'home'), BAKE_NO_UPDATE_CHECK: '1',
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

  // Update the install just made to a newer release, through its own command.
  const newer = await publishNewer(join(ROOT, 'distribution/host/public/releases', manifest.version, artifact.file), target)
  const updateEnv: NodeJS.ProcessEnv = { ...env, BAKE_RELEASE_BASE_URL: newer.base, BAKE_RELEASE_PUBLIC_KEY: newer.key }
  const bake = process.platform === 'win32' ? ['cmd.exe', '/c', join(binDir, 'bake.cmd')] : [join(binDir, 'bake')]
  const check = await run([...bake, 'update', '--check'], updateEnv, ROOT, 10)
  if (!check.includes(`Bake ${newer.version} is available`)) throw new Error(`Unexpected update check: ${check}`)
  // A manifest signed by a key the client was not told to trust is refused, and changes nothing.
  await run([...bake, 'update'], { ...updateEnv, BAKE_RELEASE_PUBLIC_KEY: trusted ?? '' }, ROOT, 1)
  const updated = await run([...bake, 'update'], updateEnv)
  if (!updated.includes(`Updated Bake ${manifest.version} → ${newer.version}`)) throw new Error(`Unexpected update output: ${updated}`)
  const upgraded = await run([...bake, '--version'], updateEnv)
  if (!upgraded.includes(newer.version)) throw new Error(`The command still starts ${upgraded.trim()} after updating`)
  const current = process.platform === 'win32'
    ? readFileSync(join(installRoot, 'current.txt'), 'utf8').trim()
    : readlinkSync(join(installRoot, 'current')).split(/[\\/]/).at(-1)
  if (current !== newer.directory) throw new Error(`current names ${current}, not ${newer.directory}`)
  if (!existsSync(join(installed, 'apps/cli/lib/bin.js'))) throw new Error('The update removed the release it replaced')
  const again = await run([...bake, 'update'], updateEnv)
  if (!again.includes(`Bake ${newer.version} is up to date`)) throw new Error(`Unexpected second update: ${again}`)
  console.log(`Verified bake update ${manifest.version} → ${newer.version} from ${newer.base}`)
} finally {
  for (const server of servers) server.kill()
  await Promise.all(servers.map(server => server.exited))
  rmSync(temporary, { recursive: true, force: true })
}
