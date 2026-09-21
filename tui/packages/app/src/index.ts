/** Cordis entry point for the interactive terminal profile. */
import type { Context } from '@deepseek-ai/cordis'
import z from '@deepseek-ai/schemastery'
import { run, type RunnerOptions, type TuiIo } from './runner.ts'
import type {} from '@deepseek-ai/dsh-cmdline'

/** Stable Cordis plugin name. */
export const name = 'tui-runner'
/** Services required before terminal setup. */
export const inject = ['agents', 'agentDefaultModel', 'sessions', 'sessionProjections', 'sessionQuery', 'commands']
/** Session choices and terminal presentation configured by the profile. */
export interface Config extends Omit<RunnerOptions, 'credentialRefs'> {
  readonly credentialRefs: string[]
}
/** Validate deployment choices and resolve defaults before starting the runner. */
export const Config: z<Config> = z.object({
  resume: z.string(),
  preset: z.string(),
  locale: z.union(['en', 'zh']).default('en'),
  doubleInterruptMs: z.number().min(1).default(500),
  credentialRefs: z.array(z.string()).default([]),
  completionLimit: z.number().min(1).step(1).default(8),
  attachmentMaxBytes: z.number().min(1).step(1).default(16 * 1024 * 1024),
  attachmentLimit: z.number().min(1).step(1).default(8),
})

/**
 * Mount one terminal owner; the launcher retains responsibility for process exit.
 * @param ctx - plugin context with the launcher exit callback.
 * @param config - validated and defaulted invocation options.
 */
export function apply(ctx: Context, config: Config): void {
  const exit = ctx.get('appExit')
  if (exit === undefined) throw new Error('tui-runner: the launcher must provide ctx.appExit')
  const io: TuiIo = { in: process.stdin, out: process.stdout, err: process.stderr, exit }
  void run(ctx, config, io).catch((error: unknown) => {
    io.err.write(`dsh: ${error instanceof Error ? error.message : String(error)}\n`)
    io.exit(1)
  })
}
