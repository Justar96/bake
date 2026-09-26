/** Terminal ownership and bounded application lifetime across session navigation. */
import React from 'react'
import { render, type Instance } from 'ink'
import type { Context } from '@deepseek-ai/cordis'
import { App } from '@dsh-tui/ui/app.tsx'
import { dictionaries, type Locale } from '@dsh-tui/ui/copy.ts'
import type { FrameStyle } from '@dsh-tui/ui/layout.ts'
import type { Clock } from '@dsh-tui/ui/activity.ts'
import { resolveFrame } from './frame.ts'
import { frameOutput } from './output.ts'
import { createSyntax } from './syntax.ts'
import type { SessionOptions } from './session.ts'
import type { AttachmentOptions } from './attachments.ts'
import { SessionNavigation } from './navigation.ts'
import { bakeVersion, releaseRoot } from './release.ts'
import { Updates } from './update.ts'
import type {} from '@deepseek-ai/cordis-plugin-loader'

/** Validated application options; no implicit defaults remain in the runner. */
export interface RunnerOptions extends SessionOptions, AttachmentOptions {
  readonly locale: Locale
  /** Profile's composer frame choice, or `auto` to read it from the terminal. */
  readonly composerFrame: FrameStyle | 'auto'
  readonly doubleInterruptMs: number
  readonly credentialRefs: readonly string[]
  readonly completionLimit: number
  /** Maximum tool-result preview lines; zero keeps only the headline and size. */
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
 * Require TTY streams and render interactively even under CI.
 *
 * Release the terminal before asynchronous shutdown.
 * @param ctx - owning plugin context.
 * @param config - resolved invocation options.
 * @param io - terminal streams and launcher exit callback.
 */
export async function run(ctx: Context, config: RunnerOptions, io: TuiIo): Promise<void> {
  if (io.in.isTTY !== true || io.out.isTTY !== true) throw new Error('tui needs an interactive terminal; use dsh --profile headless for scripted runs')
  const abort = new AbortController()
  const done = Promise.withResolvers<void>()
  let ui: Instance | undefined
  // One write per frame, drawn over the previous frame. Without synchronized
  // output, the controls would otherwise appear erased each time a line prints.
  // NO_COLOR suppresses text styling and animated indicators for this terminal.
  const motion = (process.env['NO_COLOR'] ?? '') === ''
  const output = frameOutput(io.out, io.err, motion)
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
    // Restore the terminal now, before anything else writes to it.
    output.flush()
  }
  const stop = ctx.effect(() => () => {
    abort.abort()
    releaseTerminal()
    done.resolve()
  }, 'tui terminal owner')
  const copy = dictionaries[config.locale]
  // Read once. The release does not change for the life of the process.
  const version = bakeVersion()
  const updates = new Updates({ running: version, release: releaseRoot() })
  // Loaded while the session starts, and awaited before the first frame.
  // A resumed session prints its history once. A diff drawn before the
  // grammars are ready would stay uncoloured.
  const syntax = createSyntax()
  // Resolved once, before the first frame. The terminal it describes does not
  // change for the life of the process. The presentation layer takes the
  // result instead of reading the environment itself.
  const frame = resolveFrame({ configured: config.composerFrame, locale: config.locale, env: process.env })
  // Runner state, not the controller's. The prompt outlives a session switch.
  // Routing it through `notify` put it in the same slot as command feedback,
  // where a command result cleared it and it cleared one in return.
  const interrupt = (): void => {
    if (quitTimer !== undefined) { done.resolve(); return }
    quitTimer = setTimeout(() => {
      quitTimer = undefined
      repaint()
    }, config.doubleInterruptMs)
    repaint()
  }
  const dismissQuit = (): void => {
    if (quitTimer === undefined) return
    clearTimeout(quitTimer)
    quitTimer = undefined
    repaint()
  }
  const element = (): React.ReactElement => {
    const active = navigation.controller
    if (active === undefined) throw new Error('tui: session is not connected')
    return React.createElement(App, {
      ...active.view, key: active.agent.id, inputBlocked: navigation.busy, copy, frame, clock: systemClock, motion,
      quitting: quitTimer !== undefined, completionLimit: config.completionLimit, resultLines: config.resultLines,
      highlight: syntax.highlight, version, ...updates.state === undefined ? {} : { update: updates.state },
      cwd: active.agent.session.header.cwd ?? '', sessionId: active.agent.id,
      onReferenceQuery: query => active.references.search(query),
      onArgumentQuery: query => active.argumentQuery(query),
      onInspectSubagent: id => { navigation.submit(`/agents ${id}`) },
      onCycleThinking: () => { active.cycleThinking() },
      onSubmit: text => navigation.submit(text), onCancel: () => navigation.cancel(),
      onInterrupt: interrupt, onQuitDismiss: dismissQuit, onAnswer: (id, answer) => active.interactions.answer(id, answer),
    })
  }
  const repaint = (): void => { if (!terminalReleased) ui?.rerender(element()) }
  const navigation = new SessionNavigation(ctx, config, copy, config.credentialRefs, repaint, updates)
  try {
    await ctx.get('loader')?.await()
    abort.signal.throwIfAborted()
    await navigation.start(abort.signal)
    // Before the first frame, so a known update is named from it; the network
    // request runs on behind it and never delays the session.
    updates.start(abort.signal, repaint)
    await syntax.ready
    abort.signal.throwIfAborted()
    // Incremental rendering. A frame rewrites only the lines that changed.
    // A spinner tick or a streamed token repaints one row, not every control
    // row. That is the flicker on a terminal without synchronized output.
    ui = render(element(), {
      stdin: io.in, stdout: output.out, stderr: output.err, exitOnCtrlC: false, interactive: true, incrementalRendering: true,
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
    await updates.drain()
    await syntax.close()
  }
  if (completed) io.exit(0)
}
