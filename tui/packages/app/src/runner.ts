/** Terminal ownership and bounded application lifetime across session navigation. */
import React from 'react'
import { render, type Instance } from 'ink'
import type { Context } from '@deepseek-ai/cordis'
import { App } from '@dsh-tui/ui/app.tsx'
import { dictionaries, type Locale } from '@dsh-tui/ui/copy.ts'
import type { SessionOptions } from './session.ts'
import type { AttachmentOptions } from './attachments.ts'
import { SessionNavigation } from './navigation.ts'
import type {} from '@deepseek-ai/cordis-plugin-loader'

/** Validated application options; no implicit defaults remain in the runner. */
export interface RunnerOptions extends SessionOptions, AttachmentOptions {
  readonly locale: Locale
  readonly doubleInterruptMs: number
  readonly credentialRefs: readonly string[]
  readonly completionLimit: number
}

/** Renderer streams and launcher-owned process exit. */
export interface TuiIo {
  readonly in: NodeJS.ReadStream
  readonly out: NodeJS.WriteStream
  readonly err: NodeJS.WriteStream
  readonly exit: (code: number) => void
}

/**
 * Run an interactive terminal and release the terminal before asynchronous shutdown.
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
  const interrupt = (): void => {
    if (quitTimer !== undefined) { done.resolve(); return }
    navigation.controller?.notify(copy.quit)
    quitTimer = setTimeout(() => {
      quitTimer = undefined
      navigation.controller?.notify(undefined)
    }, config.doubleInterruptMs)
  }
  const frame = (): React.ReactElement => {
    const active = navigation.controller
    if (active === undefined) throw new Error('tui: session is not connected')
    return React.createElement(App, {
      ...active.view, key: active.agent.id, inputBlocked: navigation.busy, copy, completionLimit: config.completionLimit,
      cwd: active.agent.session.header.cwd ?? '', sessionId: active.agent.id,
      onReferenceQuery: query => active.references.search(query),
      onSubmit: text => navigation.submit(text), onCancel: () => navigation.cancel(),
      onInterrupt: interrupt, onAnswer: (id, answer) => active.interactions.answer(id, answer),
    })
  }
  const navigation = new SessionNavigation(ctx, config, copy, config.credentialRefs, () => {
    if (!terminalReleased) ui?.rerender(frame())
  })
  try {
    await ctx.get('loader')?.await()
    abort.signal.throwIfAborted()
    await navigation.start(abort.signal)
    abort.signal.throwIfAborted()
    ui = render(frame(), { stdin: io.in, stdout: io.out, stderr: io.err, exitOnCtrlC: false })
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
