/** One update run checks, records the answer for the notices, and installs only a managed release. */
import { readFileSync, readlinkSync } from 'node:fs'
import { join } from 'node:path'
import { afterEach, describe, expect, it } from 'vitest'
import { CHECK_CACHE, detectInstall, hostTarget, selfUpdate, UpdateError, type InstallLayout } from '../src/index.ts'
import { managedInstall, ReleaseHost, releaseArchive, Scratch, signingKey } from './fixture.ts'

const scratches: Scratch[] = []
afterEach(() => { for (const scratch of scratches.splice(0)) scratch.dispose() })

/** Version 0.1.0 installed and running, `published` on the host for this platform. */
function setup(published: string) {
  const scratch = new Scratch()
  scratches.push(scratch)
  const key = signingKey()
  const host = new ReleaseHost()
  const installed = managedInstall(scratch, '0.1.0', releaseArchive(scratch, '0.1.0'))
  host.publish(published, hostTarget()!, releaseArchive(scratch, published), key)
  const home = join(scratch.root, 'home')
  const env = { BAKE_RELEASE_BASE_URL: host.base, BAKE_RELEASE_PUBLIC_KEY: key.publicKey }
  const run = (options: { check?: boolean; layout?: InstallLayout } = {}) => selfUpdate({
    running: '0.1.0', layout: options.layout ?? detectInstall(installed.running), env, home, fetch: host.fetch,
    ...options.check === undefined ? {} : { check: options.check },
  })
  const cache = (): unknown => JSON.parse(readFileSync(join(home, CHECK_CACHE), 'utf8'))
  return { host, home, run, cache, ...installed }
}

describe.skipIf(process.platform === 'win32')('selfUpdate', () => {
  it('installs a newer release and records it for the notices', async () => {
    const { run, root, cache } = setup('0.2.0')
    const outcome = await run()
    expect(outcome).toMatchObject({ kind: 'installed', version: '0.2.0' })
    expect(readlinkSync(join(root, 'current'))).toContain('/versions/0.2.0-')
    expect(cache()).toMatchObject({ version: '0.2.0' })
  })

  it('only reports with check, and checks from an unmanaged release too', async () => {
    const { run, root, running } = setup('0.2.0')
    await expect(run({ check: true })).resolves.toEqual({ kind: 'available', version: '0.2.0' })
    await expect(run({ check: true, layout: { kind: 'unmanaged', running: '/src/bake' } }))
      .resolves.toEqual({ kind: 'available', version: '0.2.0' })
    expect(readlinkSync(join(root, 'current'))).toBe(running)
  })

  it('refuses to install over an unmanaged release before asking the host', async () => {
    const { run, host } = setup('0.2.0')
    await expect(run({ layout: { kind: 'unmanaged', running: '/src/bake' } }))
      .resolves.toEqual({ kind: 'unmanaged', running: '/src/bake' })
    expect(host.requests).toHaveLength(0)
  })

  it('says the running release is current', async () => {
    const { run, cache } = setup('0.1.0')
    await expect(run()).resolves.toEqual({ kind: 'current', version: '0.1.0' })
    expect(cache()).not.toHaveProperty('version')
  })

  it('records a failed check so the notices retry it soon, and throws', async () => {
    const { run, host, cache } = setup('0.2.0')
    host.files.delete('latest.json.sig')
    await expect(run()).rejects.toBeInstanceOf(UpdateError)
    expect(cache()).toMatchObject({ failed: true })
  })
})
