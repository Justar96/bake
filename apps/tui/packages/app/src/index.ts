/** Cordis entry point for the interactive terminal profile. */
import type { Context } from '@deepseek-ai/cordis'
import { SESSION_IN_USE_EXIT, SessionInUseError } from 'bake-cmdline'
import z from '@deepseek-ai/schemastery'
import type { RunnerOptions, TuiIo } from './runner.ts'
import type { CredentialTargetConfig, SignInFlowConfig } from './login.ts'

/** Stable Cordis plugin name. */
export const name = 'tui-runner'
/** Services required before terminal setup. */
export const inject = ['agents', 'agentDefaultModel', 'sessions', 'sessionProjections', 'sessionQuery', 'commands']
/** Session choices and terminal presentation configured by the profile. */
export interface Config extends Omit<RunnerOptions, 'credentialRefs' | 'signInFlows'> {
  readonly credentialRefs: CredentialTargetConfig[]
  readonly signInFlows?: SignInFlowConfig[]
}
/** Validate deployment choices and resolve defaults before starting the runner. */
export const Config: z<Config> = z.object({
  resume: z.string(),
  preset: z.string(),
  // No default: set, it overrides the user's `/settings` choice, which
  // otherwise decides and starts inline.
  screen: z.union(['inline', 'fullscreen']),
  // `auto` reads the terminal. See `resolveFrame`. The explicit values are for
  // a terminal the environment describes wrongly, which is the case no
  // detection can cover.
  composerFrame: z.union(['round', 'classic', 'auto']).default('auto'),
  // Long enough to read the prompt and press again. Any other key ends it
  // sooner, so the prompt never stays over what the user went on to do.
  doubleInterruptMs: z.number().min(1).default(2000),
  // A key reference alone, or with the provider name `/login` shows, the
  // route a first sign-in selects, and the model it starts on.
  credentialRefs: z.array(z.union([z.string(), z.object({
    ref: z.string().required(), label: z.string(), provider: z.string(), model: z.string(),
  })])).default([]),
  // The authorization flows `/login` offers, by credential key, each with the
  // model a first sign-in starts on. Absent, it offers every registered flow.
  signInFlows: z.array(z.union([z.string(), z.object({ key: z.string().required(), model: z.string() })])),
  completionLimit: z.number().min(1).step(1).default(8),
  // Lines of each tool result the transcript keeps under its outcome. Zero
  // keeps output out of the terminal entirely. That is what a deployment
  // wants when it reads transcripts from a log instead of the screen.
  resultLines: z.number().min(0).step(1).default(4),
  goalObjective: z.boolean().default(false),
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
  // `run` reports its own failures on stderr as soon as they are caught,
  // before its drains run; this handler only picks the exit code once the
  // whole disposal that error triggered has settled.
  // Keep the production runner in its own artifact: loading the Ink graph as
  // part of this lightweight plugin entry would delay every other plugin.
  const runnerModule = './runner-loader' + '.js'
  void import(runnerModule).then(({ run }: typeof import('./runner-loader.ts')) => run(ctx, config, io), (error: unknown) => {
    // `run` never started, so nothing has explained the exit yet: a release
    // missing one of the runner's packages would otherwise quit silently.
    io.err.write(`dsh: could not load the terminal: ${error instanceof Error ? error.message : String(error)}\n`)
    throw error
  }).catch((error: unknown) => {
    io.exit(error instanceof SessionInUseError ? SESSION_IN_USE_EXIT : 1)
  })
}
