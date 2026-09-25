/** Standalone installers must ship the same renderer as bake update. */
import { expect, test } from 'bun:test'
import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { installerAnimation } from './embed-animation.ts'

test('both installer payloads match the maintained Node renderer', async () => {
  const code = await installerAnimation()
  for (const name of ['install.sh', 'install.ps1']) {
    const source = readFileSync(resolve(import.meta.dir, '../../distribution/host', name), 'utf8')
    expect(source.split('// # BEGIN BAKERY\n')[1]?.split('// # END BAKERY')[0]?.trim()).toBe(code)
  }
})
