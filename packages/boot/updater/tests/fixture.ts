/** Real release archives, a signing key, and a release host held in memory, all owned by one test. */
import { createHash, generateKeyPairSync, sign } from 'node:crypto'
import { execFileSync } from 'node:child_process'
import { mkdirSync, mkdtempSync, readFileSync, rmSync, symlinkSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import type { ReleaseManifest, ReleaseTarget } from '../src/index.ts'
import { directoryFor } from '../src/index.ts'

/** A throwaway Ed25519 key pair, the public half as the updater stores keys. */
export function signingKey(): { readonly publicKey: string; readonly sign: (bytes: Uint8Array) => string } {
  const pair = generateKeyPairSync('ed25519')
  return {
    publicKey: pair.publicKey.export({ type: 'spki', format: 'der' }).toString('base64'),
    sign: bytes => sign(null, bytes, pair.privateKey).toString('base64'),
  }
}

/** A temporary directory removed by {@link Scratch.dispose}. */
export class Scratch {
  readonly root = mkdtempSync(join(tmpdir(), 'bake-updater-test-'))
  dispose(): void { rmSync(this.root, { recursive: true, force: true }) }
}

/**
 * A release tree whose command prints `printed` for `--version`, packed as
 * the release archive is.
 * @returns the archive's bytes.
 */
export function releaseArchive(scratch: Scratch, printed: string): Buffer {
  const tree = mkdtempSync(join(scratch.root, 'tree-'))
  mkdirSync(join(tree, 'apps/cli/lib'), { recursive: true })
  writeFileSync(join(tree, 'apps/cli/lib/bin.js'), `console.log(${JSON.stringify(printed)})\n`)
  mkdirSync(join(tree, 'bin'))
  writeFileSync(join(tree, 'bin/bake'), '#!/bin/sh\n')
  const archive = join(scratch.root, `archive-${printed}-${Math.random().toString(16).slice(2)}.tar.gz`)
  execFileSync('tar', ['-czf', archive, '-C', tree, '.'])
  return readFileSync(archive)
}

/** An in-memory release host: `latest.json`, its signature, and archives. */
export class ReleaseHost {
  readonly files = new Map<string, Uint8Array>()
  readonly requests: string[] = []
  readonly base = 'https://releases.test'

  /**
   * Publish `version` with one archive for `target`, signed by `key`.
   * @returns the manifest as published.
   */
  publish(version: string, target: ReleaseTarget, archive: Buffer, key: ReturnType<typeof signingKey>): ReleaseManifest {
    const file = `bake-v${version}-${target}.tar.gz`
    const manifest: ReleaseManifest = {
      version,
      artifacts: { [target]: { file, sha256: createHash('sha256').update(archive).digest('hex'), size: archive.byteLength } },
    }
    const bytes = Buffer.from(`${JSON.stringify(manifest, null, 2)}\n`)
    this.files.set('latest.json', bytes)
    this.files.set('latest.json.sig', Buffer.from(`${key.sign(bytes)}\n`))
    this.files.set(`releases/${version}/${file}`, archive)
    return manifest
  }

  readonly fetch = (async (input: string | URL | Request): Promise<Response> => {
    const url = String(input instanceof Request ? input.url : input)
    this.requests.push(url)
    const body = this.files.get(url.slice(this.base.length + 1))
    return body === undefined ? new Response('missing', { status: 404 }) : new Response(body)
  }) as typeof fetch
}

/**
 * A managed install with one version, current, laid out as `install.sh` does.
 * @returns the install root and the version directory's path.
 */
export function managedInstall(scratch: Scratch, version: string, archive: Buffer): { readonly root: string; readonly running: string } {
  const root = join(scratch.root, 'install')
  const directory = directoryFor(version, createHash('sha256').update(archive).digest('hex'))
  const running = join(root, 'versions', directory)
  mkdirSync(running, { recursive: true })
  const packed = join(scratch.root, `installed-${directory}.tar.gz`)
  writeFileSync(packed, archive)
  execFileSync('tar', ['-xzf', packed, '-C', running])
  symlinkSync(running, join(root, 'current'))
  return { root, running }
}
