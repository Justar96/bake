/** Standalone installers must ship the same renderer as bake update. */
import { expect, test } from 'bun:test'
import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { installerAnimation } from './embed-animation.ts'

test('both installer payloads match the maintained Node renderer', async () => {
  const code = await installerAnimation()
  for (const name of ['install.sh', 'install.ps1']) {
    const source = readFileSync(resolve(import.meta.dir, '../../distribution/host', name), 'utf8')
    expect(source.split('// # BEGIN ANIMATION\n')[1]?.split('// # END ANIMATION')[0]?.trim()).toBe(code)
  }
})

test('the installer payload is ASCII, which Windows PowerShell reads the same in any code page', async () => {
  expect(await installerAnimation()).toMatch(/^[\x00-\x7f]*$/u)
})
