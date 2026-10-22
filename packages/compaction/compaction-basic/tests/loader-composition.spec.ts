import { mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { pathToFileURL } from 'node:url'
import { afterEach, describe, expect, it } from 'vitest'
import { Context } from '@deepseek-ai/cordis'
import Loader from '@deepseek-ai/cordis-plugin-loader'
import Include from '@deepseek-ai/cordis-plugin-include'
import LlmRuntime from 'bake-llm'
import SessionStore from 'bake-session'
import SessionProjectionRegistry from 'bake-session-projection'
import TokenMeter from 'bake-token-meter'
import BasicCompactionEngine from 'bake-compaction-basic'
import ToolResultPruner from 'bake-compaction-tool-result-pruner'
import { FileSettingsProvider } from 'bake-settings-file'

let root: string | undefined
let context: Context | undefined

afterEach(async () => {
  await context?.fiber.dispose()
  context = undefined
  if (root !== undefined) await rm(root, { recursive: true, force: true })
  root = undefined
})

async function loadYaml(
  lines: readonly string[] | ((root: string) => readonly string[]),
  files: Readonly<Record<string, string>> = {},
): Promise<Context> {
  root = await mkdtemp(join(tmpdir(), 'dsh-token-meter-loader-'))
  const configPath = join(root, 'cordis.yml')
  const composed = typeof lines === 'function' ? lines(root) : lines
  await writeFile(configPath, [...composed, ''].join('\n'))
  for (const [name, content] of Object.entries(files)) await writeFile(join(root, name), content)

  context = new Context()
  context.baseUrl = pathToFileURL(root).href + '/'
  await context.plugin(Loader)
  context.loader.builtins.include = Include
  const modules = new Map<string, unknown>([
    ['bake-llm', LlmRuntime],
    ['bake-session', SessionStore],
    ['bake-session-projection', SessionProjectionRegistry],
    ['bake-token-meter', TokenMeter],
    ['bake-compaction-tool-result-pruner', ToolResultPruner],
    ['bake-compaction-basic', BasicCompactionEngine],
    ['bake-settings-file', FileSettingsProvider],
  ])
  context.loader.internal = {
    version: 'v2',
    async import(specifier: string) {
      if (!modules.has(specifier)) throw new Error(`unexpected Loader import: ${specifier}`)
      return modules.get(specifier)
    },
  } as unknown as NonNullable<typeof context.loader.internal>
  await context.loader.create({
    name: 'cordis:include',
    config: { path: pathToFileURL(configPath).href },
  })
  await context.loader.await()
  return context
}

describe('real Loader composition', () => {
  it('loads the shipped token-meter, pruning, and compaction-basic YAML order', async () => {
    const loaded = await loadYaml([
      "- name: 'bake-llm'",
      "- name: 'bake-session'",
      "- name: 'bake-session-projection'",
      "- name: 'bake-token-meter'",
      "- name: 'bake-compaction-tool-result-pruner'",
      '  config:',
      '    thresholdChars: 100',
      '    headChars: 20',
      '    tailChars: 10',
      "- name: 'bake-compaction-basic'",
      '  config:',
      '    thresholdRatio: 0.5',
      '    retainRatio: 0.125',
      '    auto: false',
    ])

    const unloaded = [...loaded.loader.entries()]
      .filter(entry => entry.fiber === undefined && !entry.disabled)
      .map(entry => entry.options.name)
    expect(unloaded).toEqual([])
    expect(loaded.get('toolResultPruner')).toBeInstanceOf(ToolResultPruner)
    expect(loaded.get('compaction')).toBeInstanceOf(BasicCompactionEngine)
    expect((loaded.compaction as unknown as BasicCompactionEngine).config).toMatchObject({
      thresholdRatio: 0.5,
      retainRatio: 0.125,
      auto: false,
    })
  })

  it('layers a settings.yaml route-wide policy over the composed compaction entry', async () => {
    const loaded = await loadYaml(dir => [
      "- name: 'bake-llm'",
      "- name: 'bake-session'",
      "- name: 'bake-session-projection'",
      "- name: 'bake-token-meter'",
      "- name: 'bake-settings-file'",
      '  config:',
      `    path: ${JSON.stringify(join(dir, 'settings.yaml'))}`,
      '    watch: false',
      "- name: 'bake-compaction-basic'",
      '  config:',
      '    thresholdRatio: 0.7',
    ], {
      'settings.yaml': [
        'compaction-basic:',
        '  modelPolicies:',
        '    - provider: cliproxyapi',
        '      thresholdTokens: 150000',
        '      retainTokens: 30000',
        '',
      ].join('\n'),
    })

    const engine = loaded.compaction as unknown as BasicCompactionEngine
    await expect.poll(() => engine.pressureThreshold({ provider: 'cliproxyapi', model: 'gpt-5-codex' }, 200_000))
      .toBe(150_000)
    expect(engine.pressureThreshold({ provider: 'deepseek', model: 'deepseek-chat' }, 200_000)).toBe(140_000)
  })

  it('rejects stale token-meter config after Schemastery normalization', async () => {
    context = new Context()
    await context.plugin(SessionProjectionRegistry)
    await expect(context.plugin(TokenMeter, {
      contextWindow: 4096,
    } as never)).rejects.toThrow(/TokenMeterConfig: unknown key "contextWindow"/)
  })

  it('rejects stale compaction-basic config after Schemastery normalization', async () => {
    context = new Context()
    await context.plugin(LlmRuntime)
    await context.plugin(SessionStore)
    await context.plugin(SessionProjectionRegistry)
    await context.plugin(TokenMeter)
    await expect(context.plugin(BasicCompactionEngine, {
      models: { legacy: { thresholdRatio: 0.5 } },
    } as never)).rejects.toThrow(/BasicCompactionConfig: unknown key "models"/)
  })

  it('rejects a capacity-independent merged ratio conflict during plugin load', async () => {
    context = new Context()
    await context.plugin(LlmRuntime)
    await context.plugin(SessionStore)
    await context.plugin(SessionProjectionRegistry)
    await context.plugin(TokenMeter)
    await expect(context.plugin(BasicCompactionEngine, {
      retainRatio: 0.2,
      modelPolicies: [{
        provider: 'test-provider',
        model: 'test-model',
        thresholdRatio: 0.1,
      }],
    })).rejects.toThrow(/modelPolicies\[0\]: retainRatio \(0.2\).*thresholdRatio \(0.1\)/)
  })

  it('rejects an incomplete model-policy summarization pair during plugin load', async () => {
    context = new Context()
    await context.plugin(LlmRuntime)
    await context.plugin(SessionStore)
    await context.plugin(SessionProjectionRegistry)
    await context.plugin(TokenMeter)
    await expect(context.plugin(BasicCompactionEngine, {
      summarizationProvider: 'default-provider',
      summarizationModel: 'default-model',
      modelPolicies: [{
        provider: 'test-provider',
        model: 'test-model',
        summarizationModel: '',
      }],
    })).rejects.toThrow(/modelPolicies\[0\].*must be set together/)
  })
})
