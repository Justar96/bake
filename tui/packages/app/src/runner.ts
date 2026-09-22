/** Terminal ownership and bounded application lifetime across session navigation. */
import React from 'react'
import { render, type Instance } from 'ink'
import type { Context } from '@deepseek-ai/cordis'
import { App } from '@dsh-tui/ui/app.tsx'
import { dictionaries, type Locale } from '@dsh-tui/ui/copy.ts'
import type { FrameStyle } from '@dsh-tui/ui/layout.ts'
import type { Clock } from '@dsh-tui/ui/activity.ts'
import { resolveFrame } from './frame.ts'
import type { SessionOptions } from './session.ts'
import type { AttachmentOptions } from './attachments.ts'
import { SessionNavigation } from './navigation.ts'
import type {} from '@deepseek-ai/cordis-plugin-loader'

/** Validated application options; no implicit defaults remain in the runner. */
export interface RunnerOptions extends SessionOptions, AttachmentOptions {
  readonly locale: Locale
  /** Profile's composer frame choice, or `auto` to read it from the terminal. */
  readonly composerFrame: FrameStyle | 'auto'
  readonly doubleInterruptMs: number
  readonly credentialRefs: readonly string[]
  readonly completionLimit: number
  /** Tool-result lines the live region draws; the transcript keeps the size alone. */
  readonly resultLines: number
}

/** Wall-clock time and intervals for the turn header's animation and elapsed time. */
const systemClock: Clock = {
  now: () => performance.now(),
  every: (ms, tick) => {
    const timer = setInterval(tick, ms)
    return () => { clearInterval(timer) }
  },
}

/** Renderer streams and launcher-owned process exit. */
export interface TuiIo {
  readonly in: NodeJS.ReadStream
  readonly out: NodeJS.WriteStream
  readonly err: NodeJS.WriteStream
  readonly exit: (code: number) => void
}

/**
 * Require TTY streams and render interactively even under CI; release the terminal before asynchronous shutdown.
 * @param ctx - owning plugin context.
 * @param config - resolved invocation options.
 * @param io - terminal streams and launcher exit callback.
 */
export async function run(ctx: Context, config: RunnerOptions, io: TuiIo): Promise<void> {
  if (io.in.isTTY !== true || io.out.isTTY !== true) throw new Error('tui needs an interactive terminal; use dsh --profile headless for scripted runs')
  const abort = new AbortController()
  const done = Promise.withResolvers<void>()
  let ui: Instance | undefined
  let quitTimer: ReturnType<typeof setTimeout> | undefined
  let terminalReleased = false
  let completed = false
  let startupReport: Promise<void> | undefined
  const releaseTerminal = (): void => {
    if (terminalReleased) return
    terminalReleased = true
    navigation.close()
    clearTimeout(quitTimer)
    ui?.cleanup()
  }
  const stop = ctx.effect(() => () => {
    abort.abort()
    releaseTerminal()
    done.resolve()
  }, 'tui terminal owner')
  const copy = dictionaries[config.locale]
  // Resolved once, before the first frame: the terminal it describes does not
  // change for the life of the process, and the presentation layer takes the
  // answer rather than reading the environment itself.
  const frame = resolveFrame({ configured: config.composerFrame, locale: config.locale, env: process.env })
  // Motion is colour's companion: a terminal asked for no colour gets a still
  // header rather than a glyph cycling eight times a second, and keeps the
  // clock that counts the turn's seconds.
  const motion = (process.env['NO_COLOR'] ?? '') === ''
  // Runner state, not the controller's: the prompt outlives a session switch,
  // and routing it through `notify` put it in the same slot as command
  // feedback, where a command result cleared it and it cleared one back.
  const interrupt = (): void => {
    if (quitTimer !== undefined) { done.resolve(); return }
    quitTimer = setTimeout(() => {
      quitTimer = undefined
      repaint()
    }, config.doubleInterruptMs)
    repaint()
  }
  const element = (): React.ReactElement => {
    const active = navigation.controller
    if (active === undefined) throw new Error('tui: session is not connected')
    return React.createElement(App, {
      ...active.view, key: active.agent.id, inputBlocked: navigation.busy, copy, frame, clock: systemClock, motion,
      quitting: quitTimer !== undefined, completionLimit: config.completionLimit, resultLines: config.resultLines,
      cwd: active.agent.session.header.cwd ?? '', sessionId: active.agent.id,
      onReferenceQuery: query => active.references.search(query),
      onSubmit: text => navigation.submit(text), onCancel: () => navigation.cancel(),
      onInterrupt: interrupt, onAnswer: (id, answer) => active.interactions.answer(id, answer),
    })
  }
  const repaint = (): void => { if (!terminalReleased) ui?.rerender(element()) }
  const navigation = new SessionNavigation(ctx, config, copy, config.credentialRefs, repaint)
  try {
    await ctx.get('loader')?.await()
    abort.signal.throwIfAborted()
    await navigation.start(abort.signal)
    abort.signal.throwIfAborted()
    // Incremental: a frame rewrites only the lines that changed, so a spinner
    // tick or a streamed token repaints one row rather than every row of the
    // controls, which is what reads as flicker on a terminal without
    // synchronized output.
    ui = render(element(), {
      stdin: io.in, stdout: io.out, stderr: io.err, exitOnCtrlC: false, interactive: true, incrementalRendering: true,
    })
    const initial = navigation.controller!
    startupReport = initial.reportCredentials().catch((error: unknown) => {
      initial.notify(error instanceof Error ? error.message : String(error))
    })
    await Promise.race([done.promise, ui.waitUntilExit()])
    completed = !abort.signal.aborted
  } catch (error) {
    if (!abort.signal.aborted) throw error
  } finally {
    releaseTerminal()
    await stop()
    await navigation.drain()
    await startupReport
  }
  if (completed) io.exit(0)
}
