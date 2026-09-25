#!/usr/bin/env bun
/**
 * Copy verified platform archives into the Railway download service payload,
 * and sign the manifest that lists them.
 *
 * The signing key is Ed25519 PKCS#8 PEM, read from the file
 * `BAKE_RELEASE_SIGNING_KEY_FILE` names; it never enters the repository. For a
 * local or CI check, `--ephemeral-key` signs with a throwaway key instead and
 * writes its public half beside the archives, where `release:verify-local`
 * finds it. Clients trust that key only when told to through
 * `BAKE_RELEASE_PUBLIC_KEY`, and the download service's build refuses it.
 */

import { createHash, createPrivateKey, createPublicKey, generateKeyPairSync, sign, type KeyObject } from 'node:crypto'
import { createReadStream, cpSync, existsSync, mkdirSync, readdirSync, readFileSync, statSync, writeFileSync } from 'node:fs'
import { join, resolve } from 'node:path'

const ROOT = resolve(import.meta.dir, '../..')
const version = (JSON.parse(readFileSync(join(ROOT, 'apps/cli/package.json'), 'utf8')) as { version: string }).version
const rootVersion = (JSON.parse(readFileSync(join(ROOT, 'package.json'), 'utf8')) as { version: string }).version
if (version !== rootVersion) throw new Error(`Bake and CLI versions differ: ${rootVersion} != ${version}`)
const source = join(ROOT, '.artifacts/bake-release', version)
const publicRoot = join(ROOT, 'distribution/host/public')
const destination = join(publicRoot, 'releases', version)
if (!existsSync(source)) throw new Error(`No archives at ${source}; run bun run release:pack first`)

/** Where `--ephemeral-key` leaves the throwaway public key, beside the archives. */
const EPHEMERAL_PUBLIC_KEY = 'ephemeral-release-key.pub'

function signingKey(): KeyObject {
  if (process.argv.includes('--ephemeral-key')) {
    const pair = generateKeyPairSync('ed25519')
    writeFileSync(join(source, EPHEMERAL_PUBLIC_KEY), `${pair.publicKey.export({ type: 'spki', format: 'der' }).toString('base64')}\n`)
    return pair.privateKey
  }
  const path = process.env.BAKE_RELEASE_SIGNING_KEY_FILE
  if (path === undefined || path === '') {
    throw new Error('Set BAKE_RELEASE_SIGNING_KEY_FILE to the release signing key, or pass --ephemeral-key for a local check')
  }
  const key = createPrivateKey(readFileSync(path))
  if (key.asymmetricKeyType !== 'ed25519') throw new Error('The release signing key is not Ed25519')
  const published = readFileSync(join(ROOT, 'distribution/host/release-key.pub'), 'utf8').trim()
  if (createPublicKey(key).export({ type: 'spki', format: 'der' }).toString('base64') !== published) {
    throw new Error('The signing key does not match distribution/host/release-key.pub')
  }
  return key
}

const key = signingKey()
const artifacts: Record<string, { file: string; sha256: string; size: number }> = {}
const pattern = new RegExp(`^bake-v${version.replaceAll('.', '\\.')}-(darwin-(?:arm64|x64)|linux-(?:arm64|x64)|win32-x64)\\.tar\\.gz$`)
mkdirSync(destination, { recursive: true })
for (const file of readdirSync(source).sort()) {
  if (file === EPHEMERAL_PUBLIC_KEY) continue
  const target = pattern.exec(file)?.[1]
  if (target === undefined) throw new Error(`Unexpected release file: ${file}`)
  const path = join(source, file)
  const size = statSync(path).size
  if (size === 0) throw new Error(`Empty release archive: ${file}`)
  const hash = createHash('sha256')
  for await (const chunk of createReadStream(path)) hash.update(chunk)
  cpSync(path, join(destination, file))
  artifacts[target] = { file, sha256: hash.digest('hex'), size }
}
if (Object.keys(artifacts).length === 0) throw new Error('No release archives found')
if (process.argv.includes('--complete')) {
  const required = ['darwin-arm64', 'darwin-x64', 'linux-arm64', 'linux-x64', 'win32-x64']
  const missing = required.filter(target => artifacts[target] === undefined)
  if (missing.length > 0) throw new Error(`Incomplete release: missing ${missing.join(', ')}`)
}
// Signed as written: clients verify these exact bytes, not a re-serialization.
const manifest = Buffer.from(`${JSON.stringify({ version, artifacts }, null, 2)}\n`)
writeFileSync(join(publicRoot, 'latest.json'), manifest)
writeFileSync(join(publicRoot, 'latest.json.sig'), `${sign(null, manifest, key).toString('base64')}\n`)
console.log(`Staged and signed ${Object.keys(artifacts).join(', ')} for Bake ${version}`)
