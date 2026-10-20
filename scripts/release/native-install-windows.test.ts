/**
 * The shipped `distribution/host/install.ps1`: its embedded Ed25519 verifier
 * against RFC 8032's vectors, refusals, and a cross-check against
 * `node:crypto`, then the installer itself run end to end against a loopback
 * release host with an ephemeral key.
 *
 * Windows PowerShell and Windows CNG have no Ed25519 primitive and the native
 * installer runs no Node, so the verifier is the installer's own code; these
 * tests run the exact shipped region. PowerShell 7 on Linux runs the same
 * BigInteger and SHA-512 classes, so the checks run wherever a `pwsh` is
 * available: `BAKE_TEST_PWSH`, the one `.preflight/tools/pwsh` holds, or one
 * on PATH. Windows PowerShell 5.1 on Windows remains unexercised here.
 */
import { afterEach, describe, expect, test } from 'bun:test'
import { createPrivateKey, generateKeyPairSync, randomBytes, sign } from 'node:crypto'
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import {
  ed25519VerifierRegion, ephemeralKey, installerTrusting, manifestBytes, releasePath, signManifest, startReleaseHost, stubRelease,
  type HostFiles, type ReleaseHost,
} from './native-install-fixture.ts'

const roots: string[] = []
const hosts: ReleaseHost[] = []
afterEach(() => {
  for (const host of hosts.splice(0)) host.stop()
  for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true })
})

/** The PowerShell these checks run under, or why they cannot run. */
function powershell(): string | undefined {
  const named = process.env['BAKE_TEST_PWSH']
  if (named !== undefined && named !== '' && existsSync(named)) return named
  const local = resolve(import.meta.dir, '../../.preflight/tools/pwsh/pwsh')
  if (existsSync(local)) return local
  return Bun.which('pwsh') ?? Bun.which('powershell.exe') ?? undefined
}

const PWSH = powershell()
const RUN_TIMEOUT_MS = 120_000

function scratch(label: string): string {
  const root = mkdtempSync(join(tmpdir(), `bake-native-${label}-`))
  roots.push(root)
  return root
}

/** Run a PowerShell script, bounded, with its own environment. */
async function runPowerShell(
  script: string, args: readonly string[], env: Record<string, string> = {},
): Promise<{ status: number; stdout: string; stderr: string }> {
  if (PWSH === undefined) throw new Error('No PowerShell available')
  const child = Bun.spawn([PWSH, '-NoProfile', '-NonInteractive', '-File', script, ...args], {
    stdin: 'ignore', stdout: 'pipe', stderr: 'pipe', detached: process.platform !== 'win32',
    env: { PATH: process.env['PATH'] ?? '', HOME: process.env['HOME'] ?? '', ...env },
  })
  let killed = false
  const watchdog = setTimeout(() => {
    killed = true
    try { process.kill(-child.pid, 'SIGKILL') } catch { child.kill('SIGKILL') }
  }, RUN_TIMEOUT_MS)
  const [status, stdout, stderr] = await Promise.all([
    child.exited, new Response(child.stdout).text(), new Response(child.stderr).text(),
  ])
  clearTimeout(watchdog)
  if (killed) throw new Error(`PowerShell did not finish within ${String(RUN_TIMEOUT_MS)} ms\n${stdout}\n${stderr}`)
  return { status, stdout, stderr }
}

/** One signature the verifier is asked about. */
interface VerifierCase {
  readonly name: string
  readonly publicKey: string
  readonly signature: string
  readonly message: string
  readonly valid: boolean
}

/**
 * Ask the shipped verifier about every case in one PowerShell run.
 * @returns each case's answer, in order.
 */
async function verify(cases: readonly VerifierCase[]): Promise<boolean[]> {
  const directory = scratch('ed25519')
  writeFileSync(join(directory, 'verifier.ps1'), ed25519VerifierRegion())
  writeFileSync(join(directory, 'cases.json'), JSON.stringify(cases))
  writeFileSync(join(directory, 'run.ps1'), `$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'verifier.ps1')
$cases = Get-Content -Raw (Join-Path $PSScriptRoot 'cases.json') | ConvertFrom-Json
foreach ($case in $cases) {
  $answer = Test-BakeEd25519Signature (BakeHexBytes $case.publicKey) (BakeHexBytes $case.signature) (BakeHexBytes $case.message)
  Write-Output ("" + $case.name + "=" + $answer)
}
`)
  const result = await runPowerShell(join(directory, 'run.ps1'), [])
  if (result.status !== 0) throw new Error(`The verifier run exited ${String(result.status)}: ${result.stderr}`)
  const answers = result.stdout.trim().split(/\r?\n/u)
  expect(answers).toHaveLength(cases.length)
  return answers.map((line, index) => {
    const expectedName = cases[index]?.name ?? ''
    expect(line.startsWith(`${expectedName}=`)).toBe(true)
    return line.endsWith('=True')
  })
}

/** Ed25519's group order, for a signature whose S is not reduced. */
const ORDER = 2n ** 252n + 27742317777372353535851937790883648493n

function littleEndian(value: bigint, bytes: number): string {
  let text = ''
  let rest = value
  for (let index = 0; index < bytes; index++) {
    text += (rest & 0xffn).toString(16).padStart(2, '0')
    rest >>= 8n
  }
  return text
}

function flipByte(hex: string, index: number): string {
  const bytes = Buffer.from(hex, 'hex')
  const current = bytes[index]
  if (current === undefined) throw new Error(`No byte ${String(index)} to flip`)
  bytes[index] = current ^ 0x01
  return bytes.toString('hex')
}

describe.skipIf(PWSH === undefined)('the installer\'s Ed25519 verifier', () => {
  test('accepts RFC 8032 section 7.1 vectors and refuses every tampered form', async () => {
    // RFC 8032 TEST 1, TEST 2, and TEST 3.
    const vectors = [
      {
        name: 'rfc-1',
        publicKey: 'd75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a',
        signature: 'e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b',
        message: '',
      },
      {
        name: 'rfc-2',
        publicKey: '3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c',
        signature: '92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00',
        message: '72',
      },
      {
        name: 'rfc-3',
        publicKey: 'fc51cd8e6218a1a38da47ed00230f0580816ed13ba3303ac5deb911548908025',
        signature: '6291d657deec24024827e69c3abe01a30ce548a284743a445e3680d7db5ac3ac18ff9b538d16f290ae67f760984dc6594a7c15e9716ed28dc027beceea1ec40a',
        message: 'af82',
      },
    ] as const
    const third = vectors[2]
    const unreducedS = BigInt(`0x${Buffer.from(third.signature.slice(64), 'hex').reverse().toString('hex')}`) + ORDER
    const cases: VerifierCase[] = [
      ...vectors.map(vector => ({ ...vector, valid: true })),
      { ...third, name: 'flipped-signature', signature: flipByte(third.signature, 5), valid: false },
      { ...third, name: 'flipped-message', message: flipByte(third.message, 0), valid: false },
      { ...third, name: 'unreduced-s', signature: `${third.signature.slice(0, 64)}${littleEndian(unreducedS, 32)}`, valid: false },
      { ...third, name: 'wrong-key', publicKey: vectors[1].publicKey, valid: false },
      // R encoded as y = p, which is not a canonical point encoding.
      { ...third, name: 'noncanonical-r', signature: `${littleEndian(2n ** 255n - 19n, 32)}${third.signature.slice(64)}`, valid: false },
      { ...third, name: 'noncanonical-key', publicKey: littleEndian(2n ** 255n - 19n, 32), valid: false },
      { ...third, name: 'short-signature', signature: third.signature.slice(0, 126), valid: false },
      { ...third, name: 'short-key', publicKey: third.publicKey.slice(0, 62), valid: false },
    ]
    const answers = await verify(cases)
    expect(answers).toEqual(cases.map(one => one.valid))
  }, 180_000)

  test('agrees with node:crypto on random messages and refuses each with one byte changed', async () => {
    const cases: VerifierCase[] = []
    for (let index = 0; index < 50; index++) {
      const pair = generateKeyPairSync('ed25519')
      const publicKey = pair.publicKey.export({ type: 'spki', format: 'der' }).subarray(12).toString('hex')
      // One long message, which no RFC vector in this suite covers.
      const message = randomBytes(index === 0 ? 1023 : 1 + (index % 64))
      const signature = sign(null, message, pair.privateKey).toString('hex')
      cases.push({ name: `signed-${String(index)}`, publicKey, signature, message: message.toString('hex'), valid: true })
      cases.push({
        name: `tampered-${String(index)}`,
        publicKey,
        signature,
        message: flipByte(message.toString('hex'), index % message.byteLength),
        valid: false,
      })
    }
    const answers = await verify(cases)
    expect(answers).toEqual(cases.map(one => one.valid))
  }, 300_000)
})

/** A `tar.exe` for a host whose tar is called `tar`, so the Windows installer's calls resolve. */
function tarShim(directory: string): string {
  const shims = join(directory, 'shims')
  mkdirSync(shims, { recursive: true })
  const path = join(shims, 'tar.exe')
  writeFileSync(path, '#!/bin/sh\nexec tar "$@"\n')
  chmodSync(path, 0o755)
  return shims
}

describe.skipIf(PWSH === undefined)('the Windows installer', () => {
  test('installs, takes over a 0.3 launcher, keeps a user-owned command, and restores both', async () => {
    const directory = scratch('install-ps1')
    const key = ephemeralKey()
    const root = join(directory, 'root')
    const binDir = join(directory, 'bin')
    mkdirSync(binDir, { recursive: true })
    const release = stubRelease(directory, '0.4.0', 'win32-x64', { executableName: 'bake.exe' })
    const manifest = manifestBytes({ version: release.version, artifacts: { 'win32-x64': release.artifact } })
    const files: HostFiles = {
      manifest, signature: signManifest(manifest, key), releases: new Map([[releasePath(release), release.bytes]]),
    }
    const host = startReleaseHost(() => files)
    hosts.push(host)
    const installer = installerTrusting('install.ps1', key.publicKey, directory)
    const shims = process.platform === 'win32' ? '' : `${tarShim(directory)}:`
    const env = {
      PATH: `${shims}${process.env['PATH'] ?? ''}`,
      BAKE_DOWNLOAD_URL: host.origin,
      BAKE_NATIVE_ROOT: root,
      BAKE_BIN_DIR: binDir,
      BAKE_SKIP_PATH_UPDATE: '1',
    }

    // A Bake 0.3 launcher, recognized by its text, and the dsh launcher beside it.
    const legacyLauncher = '@echo off\r\nsetlocal\r\nset "BAKE_RELEASE_ROOT=C:\\Users\\x\\AppData\\Local\\Bake"\r\n'
      + 'node "%BAKE_RELEASE_ROOT%\\versions\\0.3.8-abcdef012345\\apps\\cli\\lib\\bin.js" %*\r\n'
    writeFileSync(join(binDir, 'bake.cmd'), legacyLauncher)
    writeFileSync(join(binDir, 'dsh.cmd'), legacyLauncher)

    const installed = await runPowerShell(installer, [], env)
    expect(installed.stderr).toBe('')
    expect(installed.status).toBe(0)
    expect(installed.stdout).toContain('Taking over')
    expect(readFileSync(join(root, 'current.txt'), 'utf8').trim()).toBe('0.4.0')
    expect(existsSync(join(root, 'releases/0.4.0/bin/bake.exe'))).toBe(true)
    const shim = readFileSync(join(binDir, 'bake.cmd'), 'utf8')
    expect(shim).toContain('set /p BAKE_CURRENT=<"%BAKE_NATIVE_ROOT%\\current.txt"')
    expect(shim).toContain(root)
    expect(existsSync(join(binDir, 'bake-rs.cmd'))).toBe(false)
    // The 0.3 launcher is kept as a backup, and dsh is untouched.
    expect(readFileSync(join(binDir, 'dsh.cmd'), 'utf8')).toBe(legacyLauncher)
    const links = JSON.parse(readFileSync(join(root, 'links.json'), 'utf8')) as {
      schema: string
      links: { path: string; replaced: { kind: string; backup: string } }[]
    }
    expect(links.schema).toBe('bake-native-links/1')
    expect(links.links[0]?.path).toBe(join(binDir, 'bake.cmd'))
    expect(readFileSync(links.links[0]?.replaced.backup ?? '', 'utf8')).toBe(legacyLauncher)

    const restored = await runPowerShell(installer, ['--uninstall-links'], env)
    expect(restored.status).toBe(0)
    expect(readFileSync(join(binDir, 'bake.cmd'), 'utf8')).toBe(legacyLauncher)
    expect(existsSync(join(root, 'links.json'))).toBe(false)

    // A command no Bake installer made is left alone until the install is told to replace it.
    const mine = '@echo off\r\necho someone else\r\n'
    writeFileSync(join(binDir, 'bake.cmd'), mine)
    const refused = await runPowerShell(installer, [], env)
    expect(refused.status).not.toBe(0)
    expect(`${refused.stdout}${refused.stderr}`).toContain('no Bake installer made it')
    expect(readFileSync(join(binDir, 'bake.cmd'), 'utf8')).toBe(mine)

    const replaced = await runPowerShell(installer, [], { ...env, BAKE_REPLACE_COMMAND: '1' })
    expect(replaced.status).toBe(0)
    expect(readFileSync(join(binDir, 'bake.cmd'), 'utf8')).toContain('BAKE_NATIVE_ROOT')
    expect((await runPowerShell(installer, ['--uninstall-links'], env)).status).toBe(0)
    expect(readFileSync(join(binDir, 'bake.cmd'), 'utf8')).toBe(mine)
  }, 300_000)

  test('refuses a manifest the native key did not sign and leaves nothing installed', async () => {
    const directory = scratch('install-ps1-signature')
    const key = ephemeralKey()
    const root = join(directory, 'root')
    const binDir = join(directory, 'bin')
    mkdirSync(binDir, { recursive: true })
    const release = stubRelease(directory, '0.4.0', 'win32-x64', { executableName: 'bake.exe' })
    const manifest = manifestBytes({ version: release.version, artifacts: { 'win32-x64': release.artifact } })
    // Signed by a key the installer does not carry.
    const other = createPrivateKey(generateKeyPairSync('ed25519').privateKey.export({ type: 'pkcs8', format: 'pem' }))
    const files: HostFiles = {
      manifest,
      signature: sign(null, manifest, other).toString('base64'),
      releases: new Map([[releasePath(release), release.bytes]]),
    }
    const host = startReleaseHost(() => files)
    hosts.push(host)
    const installer = installerTrusting('install.ps1', key.publicKey, directory)
    const shims = process.platform === 'win32' ? '' : `${tarShim(directory)}:`
    const result = await runPowerShell(installer, [], {
      PATH: `${shims}${process.env['PATH'] ?? ''}`,
      BAKE_DOWNLOAD_URL: host.origin,
      BAKE_NATIVE_ROOT: root,
      BAKE_BIN_DIR: binDir,
      BAKE_SKIP_PATH_UPDATE: '1',
    })
    expect(result.status).not.toBe(0)
    expect(`${result.stdout}${result.stderr}`).toContain('signature did not verify')
    expect(existsSync(join(root, 'current.txt'))).toBe(false)
    expect(existsSync(join(binDir, 'bake.cmd'))).toBe(false)
  }, 180_000)
})

test('reports whether the PowerShell installer checks ran', () => {
  if (PWSH === undefined) {
    console.info('install.ps1 checks skipped: no pwsh (set BAKE_TEST_PWSH, or unpack one under .preflight/tools/pwsh)')
  } else {
    console.info(`install.ps1 checks ran under ${PWSH}`)
  }
  expect(true).toBe(true)
})
