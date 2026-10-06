/** The scope-00 inventory regenerates deterministically and its check rejects deleted, stale, unowned, and malformed entries. */
import { describe, expect, test } from 'bun:test'
import {
  buildInventory,
  checkInventory,
  CLASSIFICATION,
  renderInventory,
  type Classification,
  type InventorySources,
  type Rule,
} from './rust-migration-inventory.ts'

const port = (scope: Rule['scopes'][number], reason: string): Rule => ({ scopes: [scope], disposition: 'port-contract', reason })

/** A small classification over the fixture tree; the real tables are exercised by `--check`. */
const TABLES: Classification = {
  packageGroups: {
    'packages/core': port('09', 'core contracts'),
    'apps/tui/packages/app': port('15', 'terminal workflows'),
  },
  packageOverrides: { 'packages/core/tool-fs': { scopes: ['08'] } },
  testAreas: [
    { id: 'scripts', prefixes: ['scripts/'], scopes: ['01'], disposition: 'replace-tooling', reason: 'workspace gates' },
    { id: 'cli-profiles', prefixes: ['apps/cli/tests/profiles/'], scopes: ['13'], disposition: 'replace-tooling', reason: 'built-profile drivers' },
  ],
  snapshotSurfaces: { session: { owner: 'snapshots/session/headless.snapshot.ts', scopes: ['02'], disposition: 'reuse-oracle', reason: 'recorded sessions' } },
  sessionFeatures: [['fs-', '08']],
  ptyScenarios: { fresh: ['workflow', ['15']], resume: ['session', ['14', '15']] },
  profiles: { tui: port('14', 'terminal profile'), headless: port('13', 'headless profile') },
  presets: { standard: port('10', 'default preset') },
  toolPackages: { 'bake-tool-fs': port('08', 'file tools'), 'bake-tool-bash': port('08', 'shell tool') },
  surfaceDirs: CLASSIFICATION.surfaceDirs,
  lanes: CLASSIFICATION.lanes.filter(lane => ['vitest-runtime', 'vitest-integration', 'vitest-tui', 'bun-scripts', 'unwired-snapshot-owner'].includes(lane.lane)),
}

const PTY = `
const FIXTURE = join(ROOT, 'snapshots/session/fs-read/session.v3.jsonl')
scenario('fresh', 'first ' + 'launch', {}, async run => {})
scenario('resume', 'exact replay', { requires: ['fresh'], replayOnly: true }, async run => {})
`

const surface = (tools: string[]): string =>
  ['# Model surface', '', '## Messages', '', '### 1. system', '', `## Tools (${tools.length})`, '', ...tools.flatMap(tool => [`### ${tool}`, '']), '## Tool history', ''].join('\n')

function fixtureFiles(): Map<string, string> {
  return new Map<string, string>([
    ['package.json', JSON.stringify({ workspaces: ['vendor/*', 'packages/*/*', 'apps/tui/packages/*'] })],
    ['vendor/cordis/package.json', JSON.stringify({ name: '@deepseek-ai/cordis' })],
    ['packages/core/agent/package.json', JSON.stringify({ name: 'bake-agent' })],
    ['packages/core/base/package.json', JSON.stringify({ name: 'bake-base' })],
    ['packages/core/headless/package.json', JSON.stringify({ name: 'bake-headless' })],
    ['packages/core/tool-fs/package.json', JSON.stringify({ name: 'bake-tool-fs' })],
    ['packages/core/tool-bash/package.json', JSON.stringify({ name: 'bake-tool-bash' })],
    ['apps/tui/packages/app/package.json', JSON.stringify({ name: 'bake-tui-app' })],
    ['packages/core/agent/tests/agent.spec.ts', ''],
    ['packages/core/agent/tests/fixtures/inputs/sample.test.ts', ''],
    ['scripts/tool.test.ts', ''],
    ['snapshots/session/headless.snapshot.ts', ''],
    ['snapshots/session/fs-read/snapshot.yml', ''],
    ['snapshots/session/fs-read/session.v3.jsonl', ''],
    ['packages/boot/app-boot/src/profile.ts', `export const PROFILE_TEMPLATES: Record<string, ProfileTemplate> = {
  tui: { bundles: ['bake-base', 'bake-tui-app'] },
  headless: { bundles: ['bake-base', 'bake-headless'] },
}`],
    ['apps/tui/scripts/pty-smoke.ts', PTY],
    ['packages/preset/agent-presets/presets/standard/preset.yml', 'name: Standard mode\n'],
    ['docs/tool-catalog.md', '# Tool Schema Catalog\n\n## Tool Package Map\n\n## `bake-tool-bash`\n\n### `bash`\n\n## `bake-tool-fs`\n\n### `read`\n\n### `write`\n'],
    ['apps/cli/tests/profiles/expected/model-surface/headless.md', surface(['bash', 'read'])],
    ['apps/cli/tests/profiles/model-surface.expected.e2e.ts', ''],
    ['apps/tui/packages/app/tests/expected/model-surface/standard.md', surface(['bash', 'read', 'write'])],
    ['apps/tui/packages/app/tests/model-surface.spec.ts', ''],
  ])
}

/** In-memory sources that record every read, so a test can prove frozen generations stay unread. */
function sources(
  files: Map<string, string>,
  order: (paths: string[]) => string[] = paths => paths,
): InventorySources & { reads: string[] } {
  const reads: string[] = []
  return {
    reads,
    files: order([...files.keys()]),
    read: (path) => {
      reads.push(path)
      const text = files.get(path)
      if (text === undefined) throw new Error(`ENOENT: ${path}`)
      return text
    },
  }
}

const committed = (files = fixtureFiles()): string => renderInventory(buildInventory(sources(files), TABLES))
const check = (files: Map<string, string>, text: string | undefined): string[] => checkInventory(sources(files), text, TABLES)

/** Rewrite one keyed section of committed inventory text. */
function editSection(text: string, section: string, edit: (items: Record<string, unknown>[]) => Record<string, unknown>[]): string {
  const document = JSON.parse(text) as Record<string, unknown>
  document[section] = edit(document[section] as Record<string, unknown>[])
  return JSON.stringify(document, null, 2)
}

describe('generation', () => {
  test('classifies every fixture item and passes its own check', () => {
    const files = fixtureFiles()
    const recorded = sources(files)
    const inventory = buildInventory(recorded, TABLES)
    expect(inventory.packages.map(item => [item.path, item.scopes])).toEqual([
      ['apps/tui/packages/app', ['15']],
      ['packages/core/agent', ['09']],
      ['packages/core/base', ['09']],
      ['packages/core/headless', ['09']],
      ['packages/core/tool-bash', ['09']],
      ['packages/core/tool-fs', ['08']],
    ])
    expect(inventory.tests.map(item => [item.path, item.lane, item.disposition])).toEqual([
      ['apps/cli/tests/profiles/model-surface.expected.e2e.ts', 'vitest-integration', 'replace-tooling'],
      ['apps/tui/packages/app/tests/model-surface.spec.ts', 'vitest-tui', 'reuse-oracle'],
      ['packages/core/agent/tests/agent.spec.ts', 'vitest-runtime', 'reuse-oracle'],
      ['scripts/tool.test.ts', 'bun-scripts', 'replace-tooling'],
      ['snapshots/session/headless.snapshot.ts', 'unwired-snapshot-owner', 'reuse-oracle'],
    ])
    expect(inventory.snapshotScenarios).toEqual([
      { path: 'snapshots/session/fs-read', surface: 'session', owner: 'snapshots/session/headless.snapshot.ts', scopes: ['02', '08'], disposition: 'reuse-oracle', basis: 'snapshots/session' },
    ])
    expect(inventory.ptyScenarios.map(item => [item.name, item.summary, item.requires, item.replayOnly])).toEqual([
      ['fresh', 'first launch', [], false],
      ['resume', 'exact replay', ['fresh'], true],
    ])
    expect(inventory.profiles.find(item => item.name === 'tui')?.surfaces).toEqual(['tui-preset:standard'])
    expect(inventory.gaps).toEqual([{ subject: 'lane:unwired-snapshot-owner', missing: '1 test files run by no repository gate: no package script or vitest config includes snapshots/' }])
    // Snapshot generations are identified by path; their bytes are never read.
    expect(recorded.reads.filter(path => path.endsWith('.jsonl'))).toEqual([])
    expect(check(files, renderInventory(inventory))).toEqual([])
  })

  test('discovery order does not change the rendering', () => {
    const files = fixtureFiles()
    const reversed = renderInventory(buildInventory(sources(files, paths => paths.reverse()), TABLES))
    const interleave = (paths: string[]): string[] => [...paths.filter((_, at) => at % 2 === 1), ...paths.filter((_, at) => at % 2 === 0)]
    const interleaved = renderInventory(buildInventory(sources(files, interleave), TABLES))
    expect(reversed).toBe(committed(files))
    expect(interleaved).toBe(committed(files))
  })

  test('an override that adds only a pending decision gets its own rule and leaves the group rule intact', () => {
    const tables: Classification = { ...TABLES, packageOverrides: { ...TABLES.packageOverrides, 'packages/core/agent': { pending: 'decide later' } } }
    const inventory = buildInventory(sources(fixtureFiles()), tables)
    expect(inventory.packages.find(item => item.path === 'packages/core/agent')?.basis).toBe('packages/core/agent')
    expect(inventory.packages.find(item => item.path === 'packages/core/base')?.basis).toBe('packages/core')
    expect(inventory.rules['packages/core']).toEqual({ reason: 'core contracts' })
    expect(inventory.rules['packages/core/agent']).toEqual({ reason: 'core contracts', pending: 'decide later' })
  })

  test('only root-runner client specs are excluded; the TUI runner still includes its own', () => {
    const files = fixtureFiles()
    files.set('packages/core/agent/tests/remote.client.spec.ts', '')
    files.set('apps/tui/packages/app/tests/remote.client.spec.ts', '')
    const tables: Classification = { ...TABLES, lanes: CLASSIFICATION.lanes }
    const lanes = new Map(buildInventory(sources(files), tables).tests.map(item => [item.path, item.lane]))
    expect(lanes.get('packages/core/agent/tests/remote.client.spec.ts')).toBe('excluded-client-spec')
    expect(lanes.get('apps/tui/packages/app/tests/remote.client.spec.ts')).toBe('vitest-tui')
  })

  test('the real classification has no rule without a reason or with an unknown scope', () => {
    const rules = [
      ...Object.values(CLASSIFICATION.packageGroups), ...CLASSIFICATION.testAreas, ...Object.values(CLASSIFICATION.snapshotSurfaces),
      ...Object.values(CLASSIFICATION.profiles), ...Object.values(CLASSIFICATION.presets), ...Object.values(CLASSIFICATION.toolPackages),
    ]
    for (const rule of rules) {
      expect(rule.reason.length).toBeGreaterThan(10)
      expect(rule.scopes.length).toBeGreaterThan(0)
    }
  })
})

describe('check rejects a committed inventory that no longer matches the sources', () => {
  test('a deleted profile or tool package entry', () => {
    const files = fixtureFiles()
    const withoutProfile = editSection(committed(files), 'profiles', items => items.filter(item => item.name !== 'tui'))
    expect(check(files, withoutProfile)).toContain('profiles: missing tui')
    const withoutTool = editSection(committed(files), 'toolPackages', items => items.filter(item => item.package !== 'bake-tool-fs'))
    expect(check(files, withoutTool)).toContain('toolPackages: missing bake-tool-fs')
    const droppedName = editSection(committed(files), 'toolPackages', items => items.map(item => (item.package === 'bake-tool-fs' ? { ...item, tools: ['read'] } : item)))
    expect(check(files, droppedName)).toContain('toolPackages: bake-tool-fs changed')
  })

  test('a test file added to or removed from the sources', () => {
    const files = fixtureFiles()
    const before = committed(files)
    files.set('packages/core/agent/tests/added.spec.ts', '')
    files.delete('scripts/tool.test.ts')
    expect(check(files, before)).toEqual(expect.arrayContaining([
      'tests: missing packages/core/agent/tests/added.spec.ts',
      'tests: stale scripts/tool.test.ts is no longer in the sources',
    ]))
  })

  test('unknown scope, disposition, or rule ownership', () => {
    const files = fixtureFiles()
    const unowned = editSection(committed(files), 'packages', items => items.map(item => (
      item.path === 'packages/core/agent' ? { ...item, scopes: ['99'], disposition: 'maybe', basis: 'nowhere' } : item)))
    expect(check(files, unowned)).toEqual(expect.arrayContaining([
      'packages packages/core/agent: unknown scope ownership ["99"]',
      'packages packages/core/agent: unknown disposition "maybe"',
      'packages packages/core/agent: basis "nowhere" names no rule',
      'packages: packages/core/agent changed',
    ]))
  })

  test('reordered items and missing or unreadable inventories', () => {
    const files = fixtureFiles()
    const reordered = editSection(committed(files), 'packages', items => items.reverse())
    expect(check(files, renderInventory(JSON.parse(reordered) as ReturnType<typeof buildInventory>))).toEqual([
      'docs/roadmap/rust-0.4/scope-00/inventory.json is not in canonical order or formatting',
    ])
    expect(check(files, undefined)).toEqual(['docs/roadmap/rust-0.4/scope-00/inventory.json is missing; run with --write'])
    expect(check(files, '{')[0]).toStartWith('docs/roadmap/rust-0.4/scope-00/inventory.json is not valid JSON')
  })

  test('malformed items and rules are reported instead of crashing the check', () => {
    const files = fixtureFiles()
    const nullItem = editSection(committed(files), 'packages', items => [...items, null as unknown as Record<string, unknown>])
    expect(check(files, nullItem)).toContain('packages[6] is not an object')
    const document = JSON.parse(committed(files)) as Record<string, unknown>
    document.rules = 'none'
    expect(check(files, JSON.stringify(document))).toContain('packages packages/core/agent: basis "packages/core" names no rule')
  })

  test('inherited object keys are not a known disposition, rule, or section', () => {
    const files = fixtureFiles()
    const inherited = editSection(committed(files), 'packages', items => items.map(item => (
      item.path === 'packages/core/agent' ? { ...item, disposition: 'toString', basis: 'constructor' } : item)))
    const document = JSON.parse(inherited) as Record<string, unknown>
    Object.assign(document, { constructor: [] })
    expect(check(files, JSON.stringify(document))).toEqual(expect.arrayContaining([
      'packages packages/core/agent: unknown disposition "toString"',
      'packages packages/core/agent: basis "constructor" names no rule',
      'unexpected section constructor',
    ]))
  })
})

describe('check fails on unclassified sources instead of defaulting them', () => {
  const cases: [string, (files: Map<string, string>) => void, string][] = [
    ['a new package group', files => files.set('packages/newgroup/thing/package.json', JSON.stringify({ name: 'bake-thing' })), 'unclassified group packages/newgroup'],
    ['a new PTY scenario', files => files.set('apps/tui/scripts/pty-smoke.ts', `${PTY}scenario('extra', 'more', {}, async run => {})\n`), 'PTY scenario extra is not classified'],
    ['a removed PTY scenario', files => files.set('apps/tui/scripts/pty-smoke.ts', PTY.replace(/scenario\('resume'.*\n/u, '')), 'no longer declares: resume'],
    ['a new preset', files => files.set('packages/preset/agent-presets/presets/fast/preset.yml', 'name: Fast\n'), 'preset fast is not classified'],
    ['a new profile template', files => files.set('packages/boot/app-boot/src/profile.ts', 'export const PROFILE_TEMPLATES = { tui: { bundles: [\'bake-base\'] }, headless: { bundles: [\'bake-base\'] }, web: { bundles: [\'bake-base\'] } }'), 'profile template web is not classified'],
    ['a deleted profile template', files => files.set('packages/boot/app-boot/src/profile.ts', 'export const PROFILE_TEMPLATES = { tui: { bundles: [\'bake-base\'] } }'), 'no longer ships: headless'],
    ['a new tool package', files => files.set('docs/tool-catalog.md', `${files.get('docs/tool-catalog.md')}\n## \`bake-tool-new\`\n\n### \`new\`\n`), 'bake-tool-new in docs/tool-catalog.md is not classified'],
    ['a deleted catalog tool still exposed by a surface', files => files.set('docs/tool-catalog.md', '## `bake-tool-bash`\n\n### `bash`\n\n## `bake-tool-fs`\n\n### `read`\n'), 'exposes write, which docs/tool-catalog.md does not list'],
    ['a test in no runner lane', files => files.set('packages/core/agent/src/inline.test.ts', ''), 'test packages/core/agent/src/inline.test.ts matches no runner lane'],
    ['a test with no owner', files => files.set('apps/cli/tests/orphan.spec.ts', ''), 'test apps/cli/tests/orphan.spec.ts has no owning package'],
    ['a snapshot surface without a rule', files => files.set('snapshots/acp/cancel/snapshot.yml', ''), 'snapshot surface acp is not in SNAPSHOT_SURFACES'],
    ['a session snapshot without a feature owner', files => files.set('snapshots/session/odd-case/snapshot.yml', ''), 'session snapshot odd-case has no SESSION_FEATURES owner'],
    // Discovered names that are inherited object keys must not resolve to prototype members.
    ['a preset named like an object key', files => files.set('packages/preset/agent-presets/presets/constructor/preset.yml', 'name: Odd\n'), 'preset constructor is not classified'],
    ['a PTY scenario named like an object key', files => files.set('apps/tui/scripts/pty-smoke.ts', `${PTY}scenario('toString', 'odd', {}, async run => {})\n`), 'PTY scenario toString is not classified'],
    ['a snapshot surface named like an object key', files => files.set('snapshots/constructor/case/snapshot.yml', ''), 'snapshot surface constructor is not in SNAPSHOT_SURFACES'],
  ]
  for (const [name, mutate, message] of cases) {
    test(name, () => {
      const files = fixtureFiles()
      const before = committed(files)
      mutate(files)
      const problems = check(files, before)
      expect(problems).toHaveLength(1)
      expect(problems[0]).toContain(message)
    })
  }
})

describe('check rejects missing or malformed source inputs', () => {
  const cases: [string, (files: Map<string, string>) => void, string][] = [
    ['a missing tool catalog', files => files.delete('docs/tool-catalog.md'), 'missing required source docs/tool-catalog.md'],
    ['a missing PTY driver', files => files.delete('apps/tui/scripts/pty-smoke.ts'), 'missing required source apps/tui/scripts/pty-smoke.ts'],
    ['missing presets', files => files.delete('packages/preset/agent-presets/presets/standard/preset.yml'), 'missing required source packages/preset/agent-presets/presets/*/preset.yml'],
    ['missing model surfaces', files => files.delete('apps/cli/tests/profiles/expected/model-surface/headless.md'), 'missing required source apps/cli/tests/profiles/expected/model-surface/*.md'],
    ['a root manifest without workspaces', files => files.set('package.json', '{}'), 'package.json has no string workspaces array'],
    ['a dynamic scenario name', files => files.set('apps/tui/scripts/pty-smoke.ts', PTY.replace('\'fresh\', \'first ', 'name, \'first ')), 'scenario name is not a static string'],
    ['a scenario registered in a loop', files => files.set('apps/tui/scripts/pty-smoke.ts', `${PTY}for (const name of names) { scenario(name, '', {}, run) }\n`), 'registered outside a top-level statement'],
    ['an unknown prerequisite', files => files.set('apps/tui/scripts/pty-smoke.ts', PTY.replace('requires: [\'fresh\']', 'requires: [\'missing\']')), 'requires unknown scenario missing'],
    ['a missing PTY fixture', files => files.delete('snapshots/session/fs-read/session.v3.jsonl'), 'PTY fixture snapshots/session/fs-read/session.v3.jsonl is missing'],
    ['a surface whose tool count disagrees', files => files.set('apps/cli/tests/profiles/expected/model-surface/headless.md', surface(['bash', 'read']).replace('Tools (2)', 'Tools (3)')), 'declares 3 tools but lists 2'],
    ['invalid preset YAML', files => files.set('packages/preset/agent-presets/presets/standard/preset.yml', 'name: [unclosed\n'), 'preset.yml is not valid YAML'],
    ['a preset without a name', files => files.set('packages/preset/agent-presets/presets/standard/preset.yml', 'order: 1\n'), 'has no preset name'],
    ['a bundle that is no workspace package', files => files.set('packages/boot/app-boot/src/profile.ts', 'export const PROFILE_TEMPLATES = { tui: { bundles: [\'bake-gone\'] }, headless: { bundles: [\'bake-base\'] } }'), 'names bundle bake-gone, which is no workspace package'],
  ]
  for (const [name, mutate, message] of cases) {
    test(name, () => {
      const files = fixtureFiles()
      const before = committed(files)
      mutate(files)
      const problems = check(files, before)
      expect(problems).toHaveLength(1)
      expect(problems[0]).toContain(message)
    })
  }

  test('a path discovered twice', () => {
    const files = fixtureFiles()
    const duplicated = sources(files, paths => [...paths, 'scripts/tool.test.ts'])
    expect(checkInventory(duplicated, committed(files), TABLES)).toEqual(['rust-migration-inventory: discovery listed scripts/tool.test.ts twice'])
  })
})
