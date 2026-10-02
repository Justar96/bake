import { Context } from '@deepseek-ai/cordis'
import { onTestFinished } from 'vitest'
import type { PtcBindingFunction, PtcBindingNamespace, PtcRunRequest } from '@deepseek-ai/dsh-ptc-runtime'
import CodemodePtcRuntime from '../src/index.ts'
import type { Config } from '../src/index.ts'

/**
 * Mount the provider on a fresh context that the current test disposes.
 * Disposal awaits every live run, so no worker outlives its test.
 * @param config - provider configuration under test.
 * @returns the context, the mounted provider, and a resolve-then-run helper.
 */
export async function mountRuntime(config: Config = {}) {
  const ctx = new Context()
  onTestFinished(async () => { await ctx.fiber.dispose() })
  await ctx.plugin(CodemodePtcRuntime, config)
  const runtime = ctx.ptcRuntime as CodemodePtcRuntime
  const run = (request: PtcRunRequest) => runtime.run(runtime.resolve(request))
  return { ctx, runtime, run }
}

/** One `tools` namespace with the `ToolCallError` contract `run_code` uses. */
export function tools(functions: Record<string, PtcBindingFunction>): PtcBindingNamespace[] {
  return [{ global: 'tools', functions, errorClass: { name: 'ToolCallError', memberNameProperty: 'toolName' } }]
}
