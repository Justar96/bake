/** The terminal app's command line: `--resume` and its `--session-id` alias name the Session to adopt. */
import { Context } from '@deepseek-ai/cordis'
import { internals as cmdlineInternals, provideCmdline } from '@deepseek-ai/dsh-cmdline'
import { afterEach, describe, expect, it } from 'vitest'
import { apply, TUI_STARTUP_SERVICE, type TuiStartupValues } from '../src/startup.ts'

const contexts: Context[] = []
afterEach(async () => {
  for (const ctx of contexts.splice(0)) await ctx.fiber.dispose()
  cmdlineInternals.stdout = process.stdout
  cmdlineInternals.stderr = process.stderr
})

function boot(args: string[]): { values: TuiStartupValues | undefined; out: string; exits: number[] } {
  const observed = { out: '', exits: [] as number[] }
  const writer = { write: (chunk: string) => { observed.out += chunk; return true } }
  cmdlineInternals.stdout = writer
  cmdlineInternals.stderr = writer
  const ctx = new Context()
  contexts.push(ctx)
  provideCmdline(ctx, { args, exit: code => void observed.exits.push(code) })
  apply(ctx)
  return { values: ctx.get(TUI_STARTUP_SERVICE) as TuiStartupValues | undefined, ...observed }
}

describe('tui command-line provider', () => {
  it.each(['--resume', '--session-id'])('adopts the Session %s names, verbatim', (flag) => {
    expect(boot([flag, ' session-x ']).values).toEqual({ resume: ' session-x ', preset: undefined })
  })

  it('rejects both spellings at once', () => {
    const { values, out, exits } = boot(['--resume', 'session-a', '--session-id', 'session-b'])
    expect(out).toContain('--resume and --session-id name the same thing; pass one')
    expect(values).toBeUndefined()
    expect(exits).toEqual([1])
  })

  it('rejects an empty identity under either spelling', () => {
    for (const flag of ['--resume', '--session-id']) {
      const { values, out } = boot([flag, ''])
      expect(out).toContain('--resume requires a non-empty session id')
      expect(values).toBeUndefined()
    }
  })

  it('names the alias in help without listing it as a second flag', () => {
    // Commander wraps descriptions to the terminal width.
    const out = boot(['--help']).out.replace(/\s+/g, ' ')
    expect(out).toContain('--resume <id>')
    expect(out).toContain('(alias: --session-id)')
    expect(out).not.toContain('--session-id <id>')
  })
})
