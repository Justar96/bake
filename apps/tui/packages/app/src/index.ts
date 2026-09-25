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
  // `auto` reads the terminal. See `resolveFrame`. The explicit values are for
  // a terminal the environment describes wrongly, which is the case no
  // detection can cover.
  composerFrame: z.union(['round', 'classic', 'auto']).default('auto'),
  // Long enough to read the prompt and press again. Any other key ends it
  // sooner, so the prompt never stays over what the user went on to do.
  doubleInterruptMs: z.number().min(1).default(2000),
  credentialRefs: z.array(z.string()).default([]),
  completionLimit: z.number().min(1).step(1).default(8),
  // Lines of each tool result the transcript keeps under its outcome. Zero
  // keeps output out of the terminal entirely. That is what a deployment
  // wants when it reads transcripts from a log instead of the screen.
  resultLines: z.number().min(0).step(1).default(4),
  attachmentMaxBytes: z.number().min(1).step(1).default(16 * 1024 * 1024),
  attachmentLimit: z.number().min(1).step(1).default(8),
})

/**
 * Mount one terminal owner. The launcher keeps responsibility for process exit.
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
