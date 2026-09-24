#!/usr/bin/env bun
/** Copy verified platform archives into the Railway download service payload. */

import { createHash } from 'node:crypto'
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

const artifacts: Record<string, { file: string; sha256: string; size: number }> = {}
const pattern = new RegExp(`^bake-v${version.replaceAll('.', '\\.')}-(darwin-(?:arm64|x64)|linux-(?:arm64|x64)|win32-x64)\\.tar\\.gz$`)
mkdirSync(destination, { recursive: true })
for (const file of readdirSync(source).sort()) {
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
writeFileSync(join(publicRoot, 'latest.json'), `${JSON.stringify({ version, artifacts }, null, 2)}\n`)
console.log(`Staged ${Object.keys(artifacts).join(', ')} for Bake ${version}`)
