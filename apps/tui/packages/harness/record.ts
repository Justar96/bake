/**
 * Session-event recorder. Appends every committed event of the first root
 * session to a newline-delimited JSON file. A real agent run then becomes a
 * fixture the component harness can replay with no harness runtime and no API key.
 *
 * Recording is raw on purpose. The fixture holds `SessionEvent` values
 * exactly as the log commits them, because that is what `project()` consumes.
 * A fixture of already-projected rows would test the renderer against the
 * renderer's own assumptions.
 *
 * Load it over any profile that runs a turn. `headless` needs no terminal.
 *
 * ```sh
 * node --import tsx/esm apps/cli/src/bin.ts --profile headless \
 *   --patch ./apps/tui/packages/harness/record.patch.yml "list the files here"
 * ```
 *
 * @module tui-recorder
 */

import { appendFileSync, mkdirSync, writeFileSync } from 'node:fs'
import { dirname } from 'node:path'
import type { Context } from '@deepseek-ai/cordis'
import z from '@deepseek-ai/schemastery'
import type { Session, SessionEvent } from '@deepseek-ai/dsh-session'

/** Cordis plugin name used by loader diagnostics. */
export const name = 'tui-recorder'

/** Plugin config. */
export interface Config {
  /** Where to write the fixture; the file is truncated when the recorder mounts. */
  path: string
}

export const Config: z<Config> = z.object({
  path: z.string().required(),
})

/**
 * Record committed events from the first root session to the configured file.
 *
 * @param ctx - the profile's Cordis context.
 * @param config - the fixture path.
 */
export function apply(ctx: Context, config: Config): void {
  mkdirSync(dirname(config.path), { recursive: true })
  // Truncate on mount so a rerun replaces the fixture instead of growing it.
  writeFileSync(config.path, '')
  let root: Session | undefined
  ctx.effect(() => ctx.on('session/event', (session: Session, event: SessionEvent) => {
    // A profile may create child sessions during the root turn. Fixtures replay
    // one displayed transcript, so keep the first non-subagent session only.
    if (session.header.origin === 'subagent') return
    root ??= session
    if (session !== root) return
    // Synchronous append. The process may exit immediately after the final
    // event, and a queued write would lose the end of the fixture.
    appendFileSync(config.path, `${JSON.stringify(event)}\n`)
  }), 'tui fixture recorder')
}
