/** Terminal ownership and bounded application lifetime around a single session. */
import React from 'react'
import { render, type Instance } from 'ink'
import type { Context } from '@deepseek-ai/cordis'
import type { AgentHandle } from '@deepseek-ai/dsh-agent'
import { App } from '@dsh-tui/ui/app.tsx'
import { dictionaries, type Locale } from '@dsh-tui/ui/copy.ts'
import { openSession, type SessionOptions } from './session.ts'
import { SessionController } from './controller.ts'
import type {} from '@deepseek-ai/cordis-plugin-loader'

/** Validated application options; no implicit defaults remain in the runner. */
export interface RunnerOptions extends SessionOptions {
  readonly locale: Locale
  readonly doubleInterruptMs: number
  readonly credentialRefs: readonly string[]
}

/** Renderer streams and launcher-owned process exit. */
export interface TuiIo {
  readonly in: NodeJS.ReadStream
  readonly out: NodeJS.WriteStream
  readonly err: NodeJS.WriteStream
  readonly exit: (code: number) => void
}

/**
 * Run one interactive session and release the terminal before asynchronous shutdown.
 * @param ctx - owning plugin context.
 * @param config - resolved invocation options.
 * @param io - terminal streams and launcher exit callback.
 */
export async function run(ctx: Context, config: RunnerOptions, io: TuiIo): Promise<void> {
  if (io.in.isTTY !== true || io.out.isTTY !== true) throw new Error('tui needs an interactive terminal; use dsh --profile headless for scripted runs')
  const abort = new AbortController()
  const done = Promise.withResolvers<void>()
  let ui: Instance | undefined
  let controller: SessionController | undefined
  let handle: AgentHandle | undefined
  let quitTimer: ReturnType<typeof setTimeout> | undefined
  let terminalReleased = false
  let completed = false
  let startupReport: Promise<void> | undefined
  const releaseTerminal = (): void => {
    if (terminalReleased) return
    terminalReleased = true
    controller?.close()
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
    controller?.notify(copy.quit)
    quitTimer = setTimeout(() => {
      quitTimer = undefined
      controller?.notify(undefined)
    }, config.doubleInterruptMs)
  }
  const frame = (): React.ReactElement => {
    if (controller === undefined) throw new Error('tui: session is not connected')
    const active = controller
    return React.createElement(App, {
      ...active.view, copy, model: `${active.agent.options.provider}/${active.agent.options.model}`,
      cwd: active.agent.session.header.cwd ?? '', sessionId: active.agent.id,
      onSubmit: text => active.submit(text), onCancel: () => active.cancel(),
      onInterrupt: interrupt, onAnswer: (id, answer) => active.interactions.answer(id, answer),
    })
  }
  try {
    await ctx.get('loader')?.await()
    abort.signal.throwIfAborted()
    handle = await openSession(ctx, config, abort.signal, agent => {
      controller = new SessionController(ctx, agent, copy, config.credentialRefs, () => {
        if (!terminalReleased) ui?.rerender(frame())
      })
    })
    if (controller === undefined) throw new Error('tui: agent setup did not connect the session')
    await controller.replay(abort.signal)
    abort.signal.throwIfAborted()
    ui = render(frame(), { stdin: io.in, stdout: io.out, stderr: io.err, exitOnCtrlC: false })
    startupReport = controller.reportCredentials().catch((error: unknown) => {
      controller?.notify(error instanceof Error ? error.message : String(error))
    })
    await Promise.race([done.promise, ui.waitUntilExit()])
    completed = !abort.signal.aborted
  } catch (error) {
    if (!abort.signal.aborted) throw error
  } finally {
    releaseTerminal()
    await stop()
    await controller?.drain()
    // The handle drains the driver and persistence before detaching its Session.
    await handle?.dispose()
    await startupReport
  }
  if (completed) io.exit(0)
}
