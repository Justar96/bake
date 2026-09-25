/** Run the real Unix installer against an isolated, signed release host. */
import { expect, test } from 'bun:test'
import { createHash, generateKeyPairSync, sign } from 'node:crypto'
import { execFileSync } from 'node:child_process'
import { mkdirSync, mkdtempSync, readFileSync, readlinkSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'

test.skipIf(process.platform === 'win32')('installer clears progress on success and signature failure, and stays plain through pipes', async () => {
  const root = mkdtempSync(join(tmpdir(), 'bake-install-animation-'))
  const tree = join(root, 'tree')
  const pair = generateKeyPairSync('ed25519')
  const target = `${process.platform}-${process.arch}`
  let reject = false
  let server: ReturnType<typeof Bun.serve> | undefined
  try {
    mkdirSync(join(tree, 'apps/cli/lib'), { recursive: true })
    mkdirSync(join(tree, 'bin'))
    writeFileSync(join(tree, 'apps/cli/lib/bin.js'), 'console.log("0.1.0")\n')
    writeFileSync(join(tree, 'bin/bake'), '#!/bin/sh\necho 0.1.0\n', { mode: 0o755 })
    const archivePath = join(root, 'release.tar.gz')
    execFileSync('tar', ['-czf', archivePath, '-C', tree, '.'])
    const archive = readFileSync(archivePath)
    const sha256 = createHash('sha256').update(archive).digest('hex')
    const file = `bake-v0.1.0-${target}.tar.gz`
    const manifest = Buffer.from(JSON.stringify({ version: '0.1.0', artifacts: { [target]: { file, sha256 } } }))
    const signature = sign(null, manifest, pair.privateKey).toString('base64')
    server = Bun.serve({ hostname: '127.0.0.1', port: 0, fetch(request) {
      const path = new URL(request.url).pathname
      if (path === '/latest.json') return new Response(manifest)
      if (path === '/latest.json.sig') return new Response(reject ? 'invalid' : signature)
      if (path === `/releases/0.1.0/${file}`) return new Response(archive)
      return new Response('', { status: 404 })
    } })
    for (const mode of ['tty', 'pipe', 'failure'] as const) {
      reject = mode === 'failure'
      const install = join(root, mode)
      let terminalOutput = ''
      const terminalClosed = Promise.withResolvers<void>()
      const child = Bun.spawn(['sh', resolve(import.meta.dir, '../../distribution/host/install.sh')], {
        env: { ...process.env, TERM: 'xterm-256color', CI: '', BAKE_NO_ANIMATION: '', NO_COLOR: '1',
          BAKE_RELEASE_BASE_URL: server.url.toString().replace(/\/$/, ''),
          BAKE_RELEASE_PUBLIC_KEY: pair.publicKey.export({ type: 'spki', format: 'der' }).toString('base64'),
          BAKE_INSTALL_ROOT: install, BAKE_BIN_DIR: join(root, `${mode}-bin`) },
        ...(mode === 'pipe' ? { stdout: 'pipe' as const, stderr: 'pipe' as const }
          : { terminal: { cols: 80, rows: 24,
            data: (_terminal, bytes) => { terminalOutput += Buffer.from(bytes).toString() },
            exit: () => terminalClosed.resolve(),
          } }),
        timeout: 20_000,
      })
      try {
        const [code, stdout, stderr] = await Promise.all([child.exited,
          child.stdout ? new Response(child.stdout).text() : '', child.stderr ? new Response(child.stderr).text() : ''])
        if (mode !== 'pipe') await terminalClosed.promise
        expect(child.signalCode).toBeNull()
        const output = terminalOutput + stdout + stderr
        if (mode === 'failure') {
          expect(code).toBe(1)
          expect(output).toContain('signature did not verify')
          expect(output).not.toContain('Freshly baked')
          expect(output.lastIndexOf('\x1b[2K')).toBeLessThan(output.indexOf('signature did not verify'))
        } else {
          expect(code).toBe(0)
          expect(readlinkSync(join(install, 'current'))).toContain(`0.1.0-${sha256.slice(0, 12)}`)
          expect(output).toContain('Installed Bake 0.1.0')
          if (mode === 'tty') expect(output).toContain('Freshly baked.')
          else expect(output).not.toContain('\x1b')
        }
      } finally {
        child.kill()
        await child.exited
        child.terminal?.close()
      }
    }
  } finally {
    await server?.stop(true)
    rmSync(root, { recursive: true, force: true })
  }
})
