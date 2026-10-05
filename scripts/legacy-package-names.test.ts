/** The legacy package-name map in `bake-app-boot` stays in step with the workspace manifests. */
import { expect, test } from 'bun:test'
import { globSync, readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import {
  currentPackageName,
  LEGACY_PACKAGE_NAMES,
  renamedModuleSpecifier,
} from '../packages/boot/app-boot/src/legacy-package-names.ts'

const ROOT = fileURLToPath(new URL('..', import.meta.url))

/** Declared names of the runtime packages under `packages/<group>/<pkg>` and the applications under `apps/`. */
function runtimePackageNames(): Set<string> {
  const manifests = ['packages/*/*/package.json', 'apps/cli/package.json', 'apps/tui/packages/*/package.json']
  return new Set(manifests.flatMap(pattern => globSync(pattern, { cwd: ROOT })).map(path => (
    JSON.parse(readFileSync(`${ROOT}/${path}`, 'utf8')) as { name: string }
  ).name))
}

test('every current name is a runtime package and no legacy name is still declared', () => {
  const names = runtimePackageNames()
  expect([...LEGACY_PACKAGE_NAMES.values()].filter(current => !names.has(current))).toEqual([])
  expect([...LEGACY_PACKAGE_NAMES.keys()].filter(legacy => names.has(legacy))).toEqual([])
})

test('every renamed runtime package keeps its upstream name as an alias', () => {
  const aliased = new Set(LEGACY_PACKAGE_NAMES.values())
  const missing = [...runtimePackageNames()].filter(name => name.startsWith('bake-') && !aliased.has(name))
  expect(missing).toEqual([])
  for (const [legacy, current] of LEGACY_PACKAGE_NAMES) {
    if (legacy.startsWith('@deepseek-ai/dsh-')) {
      expect(legacy).toBe(`@deepseek-ai/dsh-${current.slice('bake-'.length)}`)
    }
  }
})

test('maps package names and subpaths without matching name prefixes', () => {
  expect(currentPackageName('@deepseek-ai/dsh-base')).toBe('bake-base')
  expect(currentPackageName('@deepseek-ai/cordis')).toBe('@deepseek-ai/cordis')
  expect(renamedModuleSpecifier('@deepseek-ai/dsh-tool-subagent-control/list-agents'))
    .toBe('bake-tool-subagent-control/list-agents')
  expect(renamedModuleSpecifier('@deepseek-ai/dsh-session-format')).toBe('bake-session-format')
  expect(renamedModuleSpecifier('@deepseek-ai/dsh-session-formats')).toBeUndefined()
  expect(renamedModuleSpecifier('@deepseek-ai/dsh')).toBe('bake-cli')
  expect(renamedModuleSpecifier('@deepseek-ai/dsh/profile-boot')).toBe('bake-cli/profile-boot')
  expect(renamedModuleSpecifier('@dsh-tui/app/lib/runner.js')).toBe('bake-tui-app/lib/runner.js')
  expect(renamedModuleSpecifier('@dsh-tui/apps')).toBeUndefined()
  expect(renamedModuleSpecifier('bake-base')).toBeUndefined()
  expect(renamedModuleSpecifier('@deepseek-ai')).toBeUndefined()
})
