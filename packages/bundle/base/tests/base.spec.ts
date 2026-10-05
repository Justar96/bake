/**
 * The bundle's substance is its patch file: the `dsh.bundle.patch` manifest
 * field must name a real, parseable patch list.
 */

import { existsSync, readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { resolve } from 'node:path'
import { describe, expect, it, onTestFinished } from 'vitest'
import * as yaml from 'js-yaml'
import { Context } from '@deepseek-ai/cordis'
import { entryListSchema } from '@deepseek-ai/cordis-plugin-include'
import { evaluate } from '@deepseek-ai/cordis-plugin-loader'
import LlmRuntime from 'bake-llm'
import * as LlmPiAi from 'bake-llm-pi-ai'

interface BaseRow {
  id?: string
  name?: string
  config?: Record<string, unknown>
  disabled?: boolean
}

function root(): string {
  return fileURLToPath(new URL('..', import.meta.url))
}

/** The rows of the base layer's single insert list. */
function baseRows(): BaseRow[] {
  const parsed = yaml.load(readFileSync(resolve(root(), 'cordis.patch.yml'), 'utf8'), { schema: entryListSchema })
  if (!Array.isArray(parsed)) throw new TypeError('base patch must parse to a patch list')
  return (parsed as { insert?: BaseRow[] }[]).flatMap(patch => patch.insert ?? [])
}

describe('bake-base bundle', () => {
  it('declares a parseable patch list through the dsh.bundle.patch manifest field', () => {
    const root = fileURLToPath(new URL('..', import.meta.url))
    const manifest = JSON.parse(
      readFileSync(resolve(root, 'package.json'), 'utf8'),
    ) as {
      dependencies?: Record<string, string>
      dsh?: { bundle?: { patch?: string } }
    }
    expect(manifest.dsh?.bundle?.patch).toBe('./cordis.patch.yml')
    const parsed = yaml.load(
      readFileSync(resolve(root, manifest.dsh!.bundle!.patch!), 'utf8'),
      { schema: entryListSchema },
    )
    expect(Array.isArray(parsed)).toBe(true)
    // The base layer is one insert list over the empty profile root.
    const rows = (parsed as { insert?: { id?: string; config?: Record<string, unknown>; disabled?: boolean }[] }[]).flatMap(
      patch => patch.insert ?? [],
    )
    expect(rows.length).toBeGreaterThan(50)
    expect(rows.some(row => row.id === 'agent-loop')).toBe(true)
    expect(rows.find(row => row.id === 'session-telemetry-otel')?.disabled).toBeUndefined()
    expect(rows.find(row => row.id === 'hmr')).toMatchObject({
      config: { root: [] },
    })
    expect(rows.filter(row => row.id === 'subagent-codex')).toHaveLength(0)
    expect(rows.filter(row => row.id === 'subagent-claude-code')).toHaveLength(0)
    expect(rows.find(row => row.id === 'web')?.config).toMatchObject({ fetchProvider: 'http' })
    expect(rows.find(row => row.id === 'web-fetch-http')).toBeDefined()
    expect(rows.find(row => row.id === 'tool-web')?.config).toMatchObject({ fetch: true })
    expect(manifest.dependencies).not.toHaveProperty('@deepseek-ai/dsh-subagent-codex')
    expect(manifest.dependencies).not.toHaveProperty('@deepseek-ai/dsh-subagent-claude-code')
    expect(manifest.dependencies).toHaveProperty('bake-web-fetch-http')
  })

  it('uploads sessions only to an OTLP endpoint the user configures', () => {
    const text = readFileSync(resolve(root(), 'cordis.patch.yml'), 'utf8')
    expect(text).not.toMatch(/deepseeksvc|harness-telemetry/u)
    expect(text).toContain("# TODO(bake): Bake's own OTLP logs endpoint goes here")
    const row = baseRows().find(candidate => candidate.id === 'session-telemetry-otel')
    const config = row?.config as { mode?: { __jsExpr?: string }; exporter?: { url?: { __jsExpr?: string } } } | undefined
    const mode = config?.mode?.__jsExpr
    const url = config?.exporter?.url?.__jsExpr
    if (mode === undefined || url === undefined) throw new Error('the telemetry row must derive mode and url from the environment')
    const resolveWith = (env: Record<string, string>) => ({
      mode: evaluate({ process: { env } }, mode) as unknown,
      url: evaluate({ process: { env } }, url) as unknown,
    })
    // No endpoint: nothing to upload to, whatever mode was asked for.
    expect(resolveWith({})).toEqual({ mode: 'DISABLED', url: '' })
    expect(resolveWith({ DSH_TELEMETRY_OTLP_URL: '' })).toEqual({ mode: 'DISABLED', url: '' })
    expect(resolveWith({ DSH_TELEMETRY_MODE: 'FEEDBACK_ONLY' })).toEqual({ mode: 'DISABLED', url: '' })
    // A configured endpoint enables upload to exactly that URL, in the requested mode.
    const endpoint = 'http://127.0.0.1:4318/v1/logs'
    expect(resolveWith({ DSH_TELEMETRY_OTLP_URL: endpoint })).toEqual({ mode: 'FEEDBACK_ONLY', url: endpoint })
    expect(resolveWith({ DSH_TELEMETRY_OTLP_URL: endpoint, DSH_TELEMETRY_MODE: 'DISABLED' }))
      .toEqual({ mode: 'DISABLED', url: endpoint })
  })

  it('serves DeepSeek through the pi-ai adapter with its in-history capabilities', async () => {
    const rows = baseRows()
    expect(rows.map(row => row.name)).not.toContain('@deepseek-ai/dsh-llm-deepseek')
    const row = rows.find(candidate => candidate.id === 'llm-pi-ai')
    expect(row?.name).toBe('bake-llm-pi-ai')
    const ctx = new Context()
    onTestFinished(() => ctx.fiber.dispose())
    await ctx.plugin(LlmRuntime)
    await ctx.plugin(LlmPiAi, row?.config as LlmPiAi.Config)

    expect(ctx.llm.listProviders()).toContainEqual({ id: 'deepseek-official', name: 'DeepSeek' })
    expect((await ctx.llm.listModels('deepseek-official')).map(model => model.id))
      .toEqual(['deepseek-flash', 'deepseek-v4-pro'])
    const flash = await ctx.llm.resolveModelInfo('deepseek-official', 'deepseek-flash')
    expect(flash).toMatchObject({
      inputModalities: ['text', 'image'],
      context: { contextWindow: 1_000_000 },
      defaultMaxTokens: 256_000,
      systemPromptUpdate: 'in-history',
      toolUpdate: 'in-history',
      reasoning: { defaultEffort: 'high' },
    })
    expect(flash.reasoning?.efforts.map(effort => effort.id)).toEqual(['off', 'low', 'high', 'max'])
    const pro = await ctx.llm.resolveModelInfo('deepseek-official', 'deepseek-v4-pro')
    expect(pro).toMatchObject({ inputModalities: ['text'], defaultMaxTokens: 256_000 })
    expect(pro.description).toBeTypeOf('string')
    expect(pro).not.toHaveProperty('systemPromptUpdate')
    expect(pro).not.toHaveProperty('toolUpdate')
  })

  it('gates each shell stack by platform with a symmetric disabled expression', () => {
    const root = fileURLToPath(new URL('..', import.meta.url))
    const parsed = yaml.load(
      readFileSync(resolve(root, 'cordis.patch.yml'), 'utf8'),
      { schema: entryListSchema },
    )
    if (!Array.isArray(parsed)) throw new TypeError('base patch must parse to a patch list')
    const rows = parsed.flatMap((patch): Record<string, unknown>[] =>
      typeof patch === 'object' && patch !== null
        ? (patch as { insert?: Record<string, unknown>[] }).insert ?? []
        : [],
    )
    // Symmetric gating: each stack's executor and tool rows carry the same
    // platform fact, inverted between the bash and pwsh twins, so exactly one
    // shell stack mounts per host. Evaluate with a platform-scoped context
    // (the `with` scope shadows the global `process`) so both outcomes pin on
    // every host.
    for (const [id, win32, linux] of [
      ['bash-sandbox', true, false],
      ['tool-bash', true, false],
      ['pwsh-sandbox', false, true],
      ['tool-pwsh', false, true],
    ] as const) {
      const row = rows.find(candidate => candidate.id === id)
      if (row === undefined) throw new Error(`base patch must mount ${id}`)
      const expression = (row.disabled as { __jsExpr?: string } | undefined)?.__jsExpr
      if (expression === undefined) throw new Error(`${id} must gate on a !!js disabled expression`)
      expect(Boolean(evaluate({ process: { platform: 'win32' } }, expression)), `${id} on win32`).toBe(win32)
      expect(Boolean(evaluate({ process: { platform: 'linux' } }, expression)), `${id} on linux`).toBe(linux)
    }
    // The platform layer folded into these rows: no separate patch file ships.
    expect(existsSync(resolve(root, 'windows.cordis.patch.yml'))).toBe(false)
  })
})
