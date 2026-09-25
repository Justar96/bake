import { createHash, createPublicKey, verify } from 'node:crypto'
import { createReadStream, readFileSync, statSync } from 'node:fs'

const publicRoot = new URL('./public/', import.meta.url)
const bytes = readFileSync(new URL('latest.json', publicRoot))
// The committed release key, plus one a local check names in the environment.
// The service's image build sets no environment, so it accepts only the real key.
const keys = [readFileSync(new URL('release-key.pub', import.meta.url), 'utf8').trim(), process.env.BAKE_RELEASE_PUBLIC_KEY?.trim()]
  .filter(key => key !== undefined && key !== '')
let signature
try { signature = Buffer.from(readFileSync(new URL('latest.json.sig', publicRoot), 'utf8').trim(), 'base64') } catch { throw new Error('Missing release manifest signature') }
const signed = signature.length === 64 && keys.some(key => {
  try { return verify(null, bytes, createPublicKey({ key: Buffer.from(key, 'base64'), format: 'der', type: 'spki' }), signature) } catch { return false }
})
if (!signed) throw new Error('Release manifest signature does not verify')
const manifest = JSON.parse(bytes.toString('utf8'))
if (!/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(manifest.version)) throw new Error('Invalid release version')
const supported = ['darwin-arm64', 'darwin-x64', 'linux-arm64', 'linux-x64', 'win32-x64']
const artifacts = manifest.artifacts
if (artifacts === null || typeof artifacts !== 'object' || Array.isArray(artifacts)
  || Object.keys(artifacts).length === 0) throw new Error('No release artifacts')
const available = Object.keys(artifacts)
for (const target of available) {
  if (!supported.includes(target)) throw new Error(`Unsupported release target: ${target}`)
}
const targets = process.argv.includes('--complete') ? supported : available
for (const target of targets) {
  const artifact = artifacts[target]
  if (artifact?.file !== `bake-v${manifest.version}-${target}.tar.gz`
    || !/^[a-f0-9]{64}$/.test(artifact.sha256)) throw new Error(`Missing or invalid ${target} artifact`)
  if (!Number.isSafeInteger(artifact.size) || artifact.size <= 0) throw new Error(`Invalid archive size for ${target}`)
  const file = new URL(`releases/${manifest.version}/${artifact.file}`, publicRoot)
  if (statSync(file).size !== artifact.size) throw new Error(`Size mismatch for ${target}`)
  const hash = createHash('sha256')
  for await (const chunk of createReadStream(file)) hash.update(chunk)
  if (hash.digest('hex') !== artifact.sha256) throw new Error(`SHA-256 mismatch for ${target}`)
}
console.log(`Verified Bake ${manifest.version} release: ${targets.join(', ')}`)
