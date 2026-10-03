import { readFileSync } from 'node:fs'
import { createRequire } from 'node:module'
import * as yaml from 'js-yaml'
import { Context } from '@deepseek-ai/cordis'
import { entryListSchema } from '@deepseek-ai/cordis-plugin-include'
import type { SessionEvent } from '@deepseek-ai/dsh-session'
import type { Agent } from '@deepseek-ai/dsh-agent'
import AgentLoop from '@deepseek-ai/dsh-agent-loop'
import { mountAgentLoopTestDependencies } from '@deepseek-ai/dsh-agent-loop-testkit'
import { LocalBashExecutor } from '@deepseek-ai/dsh-bash-local'
import * as BashEnvPlugin from '@deepseek-ai/dsh-shell-env'
import LocalSubprocessRuntime from '@deepseek-ai/dsh-subprocess-local'
import * as ToolBash from '@deepseek-ai/dsh-tool-bash'
import * as LlmPiAi from '@deepseek-ai/dsh-llm-pi-ai'
import type { PiAiProviderProfile } from '@deepseek-ai/dsh-llm-pi-ai'
import TokenMeter from '@deepseek-ai/dsh-token-meter'
import ToolResultPruner from '@deepseek-ai/dsh-compaction-tool-result-pruner'
import JsonlSessionPersistence from '@deepseek-ai/dsh-session-persistence-jsonl'
import * as SessionCheckpointPolicy from '@deepseek-ai/dsh-session-checkpoint-policy'
import { BasicCompactionEngine } from '@deepseek-ai/dsh-compaction-basic'
import type { BasicCompactionConfig } from '@deepseek-ai/dsh-compaction-basic'

/**
 * Shared harness for the headless-agent e2e suites: the full plugin stack
 * with the shipped DeepSeek route and the real bash tool. Lives
 * outside the *.e2e.ts pattern so importing it never re-registers another
 * file's tests.
 */

export const SYSTEM_PROMPT = 'You are a coding agent. Use bash for file operations '
  + 'with cat/grep/heredocs; check [exit code: N] markers, '
  + 'and report results briefly.'

/** Options for {@link codingHarness}. */
export interface CodingHarnessOptions {
  /**
   * Deployment persona prefix for the tree (the system-prompt plugin's `personaPrefix`
   * config — per-context, not per-agent). Omitted ⇒ no persona prefix section.
   */
  personaPrefix?: string
  /** Durable JSONL persistence root (the resume suite needs it; others stay file-free). */
  persistenceRoot?: string
  /**
   * Load {@link BasicCompactionEngine} with this config so the compaction e2e can
   * trigger compaction at a small, controlled history size. Omitted ⇒ no
   * compaction plugin (the default suites run without it).
   */
  compact?: BasicCompactionConfig
  /** Test-only context capacity advertised for `deepseek-flash`. */
  modelContextWindow?: number
}

/** The `deepseek-official` profile in the base bundle's `llm-pi-ai` row, parsed once. */
const SHIPPED_DEEPSEEK: PiAiProviderProfile = (() => {
  const patchPath = createRequire(import.meta.url).resolve('@deepseek-ai/dsh-base/cordis.patch.yml')
  const patches = yaml.load(readFileSync(patchPath, 'utf8'), { schema: entryListSchema }) as { insert?: { id?: string; config?: unknown }[] }[]
  const row = patches.flatMap(patch => patch.insert ?? []).find(entry => entry.id === 'llm-pi-ai')
  const profile = (row?.config as { providers?: Record<string, PiAiProviderProfile> } | undefined)?.providers?.['deepseek-official']
  if (profile === undefined) throw new Error('dsh-base cordis.patch.yml ships no llm-pi-ai deepseek-official profile')
  return profile
})()

/**
 * The `deepseek-official` route the base bundle ships, read from
 * packages/bundle/base/cordis.patch.yml, for suites that mount the adapter by
 * hand.
 * @param contextWindow - test-only capacity for `deepseek-flash`.
 * @returns a fresh profile the caller may mutate.
 */
export function deepseekProfile(contextWindow?: number): PiAiProviderProfile {
  const profile = structuredClone(SHIPPED_DEEPSEEK)
  if (contextWindow === undefined) return profile
  return {
    ...profile,
    models: profile.models?.map(model => model.id === 'deepseek-flash' ? { ...model, contextWindow } : model),
  }
}

export async function codingHarness(workdir: string, options: CodingHarnessOptions = {}): Promise<Context> {
  const ctx = new Context()
  await mountAgentLoopTestDependencies(ctx, {
    systemPrompt: { personaPrefix: options.personaPrefix ?? '' },
  })
  await ctx.plugin(AgentLoop, { agents: [] })
  await ctx.plugin(LlmPiAi, {
    providers: { 'deepseek-official': deepseekProfile(options.modelContextWindow) },
  })
  await ctx.plugin(LocalSubprocessRuntime)
  await ctx.plugin(BashEnvPlugin)
  await ctx.plugin(LocalBashExecutor, { cwd: workdir, timeoutMs: 30_000 })
  await ctx.plugin(ToolBash)
  // Compaction is opt-in: only the compaction e2e loads the reusable meter and backend.
  if (options.compact !== undefined) {
    await ctx.plugin(TokenMeter)
    await ctx.plugin(ToolResultPruner)
    await ctx.plugin(BasicCompactionEngine, options.compact)
  }
  // Durable JSONL persistence is opt-in: only the resume e2e needs it, and the
  // other suites stay file-free. Loaded last so a resume's deferred
  // `ctx.inject(['sessionPersistence'])` resolves once this is present.
  if (options.persistenceRoot !== undefined) {
    await ctx.plugin(JsonlSessionPersistence, { root: options.persistenceRoot })
    await ctx.plugin(SessionCheckpointPolicy)
  }
  return ctx
}

export function waitForIdle(ctx: Context, agent: Agent): Promise<void> {
  return new Promise((resolve) => {
    const dispose = ctx.on('agent/status', ({ agent: subject, status }) => {
      if (subject === agent && status === 'idle') {
        dispose()
        resolve()
      }
    })
  })
}

export function finalText(events: readonly SessionEvent[]): string {
  const message = events.findLast(event => event.type === 'assistant/message')
  if (message?.type !== 'assistant/message') return ''
  return message.data.message.content
    .filter(block => block.type === 'text')
    .map(block => block.text)
    .join('')
}
