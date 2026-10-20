// The native release gate of the download service's image build: the native
// manifest must be signed by the committed native key and describe the
// archives this image holds. It mirrors verify-manifest.mjs for the native
// route, which no 0.3 updater reads, and trusts the native key alone.
import { createHash, createPublicKey, verify } from 'node:crypto'
import { createReadStream, existsSync, readFileSync, statSync } from 'node:fs'

const publicRoot = new URL('./public/native/', import.meta.url)
const manifestFile = new URL('stable.json', publicRoot)
const required = process.argv.includes('--required')
if (!existsSync(manifestFile)) {
  if (required) throw new Error('Missing public/native/stable.json')
  console.log('No native release manifest; nothing to verify')
  process.exit(0)
}
// A manifest must not be served while the signing key is a placeholder whose
// private half was discarded; see native-release-key.placeholder.
if (existsSync(new URL('native-release-key.placeholder', import.meta.url))) {
  throw new Error('native-release-key.pub is still a placeholder; a native manifest cannot be published under it')
}
const bytes = readFileSync(manifestFile)
// The committed native key, plus one a local check names in the environment.
// The service's image build sets no environment, so it accepts only the real key.
const keys = [readFileSync(new URL('native-release-key.pub', import.meta.url), 'utf8').trim(), process.env.BAKE_NATIVE_RELEASE_PUBLIC_KEY?.trim()]
  .filter(key => key !== undefined && key !== '')
let signature
try { signature = Buffer.from(readFileSync(new URL('stable.json.sig', publicRoot), 'utf8').trim(), 'base64') } catch { throw new Error('Missing native release manifest signature') }
const signed = signature.length === 64 && keys.some(key => {
  try { return verify(null, bytes, createPublicKey({ key: Buffer.from(key, 'base64'), format: 'der', type: 'spki' }), signature) } catch { return false }
})
if (!signed) throw new Error('Native release manifest signature does not verify')
let manifest
try { manifest = JSON.parse(bytes.toString('utf8')) } catch { throw new Error('The native release manifest is not JSON') }
if (manifest === null || typeof manifest !== 'object' || Array.isArray(manifest)) throw new Error('The native release manifest is not an object')
// Exactly these members, in any order; an unknown one means another writer.
const members = ['schema', 'channel', 'version', 'published', 'artifacts']
const unknown = Object.keys(manifest).filter(key => !members.includes(key))
if (unknown.length > 0) throw new Error(`Unknown native manifest members: ${unknown.sort().join(', ')}`)
const missing = members.filter(key => !(key in manifest))
if (missing.length > 0) throw new Error(`Native manifest lacks ${missing.join(', ')}`)
if (manifest.schema !== 'bake-native-release/1') throw new Error(`Unsupported native manifest schema: ${JSON.stringify(manifest.schema)}`)
if (manifest.channel !== 'stable') throw new Error(`Unsupported native release channel: ${JSON.stringify(manifest.channel)}`)
if (typeof manifest.version !== 'string' || !/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(manifest.version)) throw new Error('Invalid native release version')
// The round trip refuses a date that exists only by rollover, such as 2026-02-30.
if (typeof manifest.published !== 'string' || !/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$/.test(manifest.published)
  || Number.isNaN(Date.parse(manifest.published))
  || `${new Date(manifest.published).toISOString().slice(0, 19)}Z` !== manifest.published) throw new Error('Invalid native release published time')
const supported = ['darwin-arm64', 'darwin-x64', 'linux-arm64', 'linux-x64', 'win32-x64']
const artifacts = manifest.artifacts
if (artifacts === null || typeof artifacts !== 'object' || Array.isArray(artifacts)
  || Object.keys(artifacts).length === 0) throw new Error('No native release artifacts')
const available = Object.keys(artifacts)
for (const target of available) {
  if (!supported.includes(target)) throw new Error(`Unsupported native release target: ${target}`)
}
const targets = process.argv.includes('--complete') ? supported : available
// The service image holds no archives; its server redirects to the GitHub release.
const allowMissing = process.argv.includes('--allow-missing-archives')
const held = []
for (const target of targets) {
  const artifact = artifacts[target]
  if (artifact === null || typeof artifact !== 'object' || Array.isArray(artifact)) throw new Error(`Missing or invalid ${target} artifact`)
  const extra = Object.keys(artifact).filter(key => !['file', 'sha256', 'size'].includes(key))
  if (extra.length > 0) throw new Error(`Unknown ${target} artifact members: ${extra.sort().join(', ')}`)
  if (artifact.file !== `bake-v${manifest.version}-${target}.tar.gz`
    || typeof artifact.sha256 !== 'string' || !/^[a-f0-9]{64}$/.test(artifact.sha256)) throw new Error(`Missing or invalid ${target} artifact`)
  if (!Number.isSafeInteger(artifact.size) || artifact.size <= 0) throw new Error(`Invalid archive size for ${target}`)
  const file = new URL(`releases/${manifest.version}/${artifact.file}`, publicRoot)
  if (allowMissing && !existsSync(file)) {
    held.push(target)
    continue
  }
  if (statSync(file).size !== artifact.size) throw new Error(`Size mismatch for ${target}`)
  const hash = createHash('sha256')
  for await (const chunk of createReadStream(file)) hash.update(chunk)
  if (hash.digest('hex') !== artifact.sha256) throw new Error(`SHA-256 mismatch for ${target}`)
}
console.log(`Verified native Bake ${manifest.version} release: ${targets.join(', ')}${held.length > 0 ? `; archives held elsewhere: ${held.join(', ')}` : ''}`)
