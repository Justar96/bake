import { createHash } from 'node:crypto'
import { createReadStream, readFileSync, statSync } from 'node:fs'

const publicRoot = new URL('./public/', import.meta.url)
const manifest = JSON.parse(readFileSync(new URL('latest.json', publicRoot), 'utf8'))
if (!/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(manifest.version)) throw new Error('Invalid release version')
const supported = new Set(['darwin-arm64', 'darwin-x64', 'linux-arm64', 'linux-x64', 'win32-x64'])
const artifacts = manifest.artifacts
if (artifacts === null || typeof artifacts !== 'object' || Array.isArray(artifacts)
  || Object.keys(artifacts).length === 0) throw new Error('Release has no artifacts')
for (const [target, artifact] of Object.entries(artifacts)) {
  if (!supported.has(target)) throw new Error(`Unsupported release target: ${target}`)
  if (artifact?.file !== `bake-v${manifest.version}-${target}.tar.gz`
    || !/^[a-f0-9]{64}$/.test(artifact.sha256)
    || !Number.isSafeInteger(artifact.size) || artifact.size <= 0) throw new Error(`Invalid ${target} artifact`)
  const file = new URL(`releases/${manifest.version}/${artifact.file}`, publicRoot)
  if (statSync(file).size !== artifact.size) throw new Error(`Size mismatch for ${target}`)
  const hash = createHash('sha256')
  for await (const chunk of createReadStream(file)) hash.update(chunk)
  if (hash.digest('hex') !== artifact.sha256) throw new Error(`SHA-256 mismatch for ${target}`)
}
console.log(`Verified Bake ${manifest.version} release: ${Object.keys(artifacts).join(', ')}`)
