/**
 * The TUI's command-line provider. It parses the terminal app's own flags and
 * publishes {@link TUI_STARTUP_SERVICE}. The runner is an ordinary consumer
 * whose lazy config waits for that service, so Loader resolves the runner's
 * `!!js` expressions only after the flags exist.
 *
 * @module @dsh-tui/app/startup
 */

import { Command, Option } from 'commander'
import type { Context } from '@deepseek-ai/cordis'
import { parseCmdline } from '@deepseek-ai/dsh-cmdline'

/** Stable Cordis plugin name. */
export const name = 'tui-startup'

/** Services required before the invocation can be resolved. */
export const inject = ['cmdlineArgs']

/** Service provided by this plugin and injected by the runner row. */
export const TUI_STARTUP_SERVICE = 'tuiStartup'

/** What the runner row reads from {@link TUI_STARTUP_SERVICE}. */
export interface TuiStartupValues {
  /** Session identity to adopt and replay. Absent starts a fresh session. */
  resume: string | undefined
  /** Agent preset to mount. Absent leaves the profile's own composition in place. */
  preset: string | undefined
}

/**
 * This app's command, its options, and its help text.
 *
 * @returns a fresh program, so one process can parse more than once in tests.
 */
function tuiCommand(): Command {
  return new Command()
    .name('dsh --profile tui')
    .description('Work with the agent in an interactive terminal session.')
    .helpOption('-h, --help', 'show this help')
    .option('--resume <id>', 'adopt and replay the persisted Session with this id (alias: --session-id)')
    // The headless profile's spelling, so one flag resumes a Session in either app.
    .addOption(new Option('--session-id <id>').hideHelp())
    .option('--preset <name>', 'mount this agent preset instead of the profile default')
    .addHelpText('after', `
Examples:
  dsh tui                        start a new session
  dsh tui --resume session-…     continue a stored session
  dsh tui --preset minimal       start with a smaller tool roster
`)
}

/**
 * Parse the invocation and provide it as an ordinary Cordis service.
 *
 * @param ctx - plugin context carrying the command line.
 */
export function apply(ctx: Context): void {
  const program = tuiCommand()
  program.action(() => {
    const options = program.opts<{ resume?: string, sessionId?: string, preset?: string }>()
    if (options.resume !== undefined && options.sessionId !== undefined) {
      program.error('error: --resume and --session-id name the same thing; pass one')
    }
    // A SessionId is opaque, so whitespace belongs to the identity. Reject an
    // empty value, but hand the runner the exact string it was given.
    const resume = options.resume ?? options.sessionId
    if (resume !== undefined && resume.trim() === '') {
      program.error('error: --resume requires a non-empty session id')
    }
    const preset = options.preset
    if (preset !== undefined && preset.trim() === '') {
      program.error('error: --preset requires a non-empty preset name')
    }
    ctx.provide(TUI_STARTUP_SERVICE, { resume, preset } satisfies TuiStartupValues)
  })
  parseCmdline(ctx, program)
}
