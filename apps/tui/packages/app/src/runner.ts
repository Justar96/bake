/** Terminal ownership and bounded application lifetime across session navigation. */
import React, { useEffect, type ComponentType } from 'react'
import { render, useApp, type Instance } from 'ink'
import type { Context } from '@deepseek-ai/cordis'
import { compactPath } from '@dsh-tui/ui/present.ts'
import type { Highlight } from '@dsh-tui/ui/present.ts'
import { dictionaries, type TuiCopy } from '@dsh-tui/ui/copy.ts'
import type { FrameStyle } from '@dsh-tui/ui/layout.ts'
import type { Clock } from '@dsh-tui/ui/activity.ts'
import { resolveFrame } from './frame.ts'
import { frameOutput, type FrameOutput } from './output.ts'
import { editExternally, holdInput, type EditText, type SuspendTerminal } from './external-editor.ts'
import { Preferences } from './preferences.ts'
import type { Syntax } from './syntax.ts'
import type { SessionOptions } from './session.ts'
import type { AttachmentOptions } from './attachments.ts'
import type { AppProps } from '@dsh-tui/ui/app.tsx'
import { SessionNavigation } from './navigation.ts'
import { bakeVersion, releaseRoot } from './release.ts'
import { Updates } from './update.ts'
import { sessionGitConfinement, WorkspaceGit } from './git.ts'
import { cliProxyModelsInUse, cliProxyUpgradeNotice, refreshCliProxyModels, upgradeCliProxyRoute } from './cliproxyapi.ts'
import type { CredentialTargetConfig, LoginSources, SignInFlowConfig } from './login.ts'
import type {} from '@deepseek-ai/cordis-plugin-loader'
// Declares the launcher's `app/unhandled-rejection` event.
import type {} from '@deepseek-ai/dsh-cmdline'

/** The renderer component is loaded from its own artifact after the runner starts. */
type AppComponent = ComponentType<AppProps>

/**
 * Validated application options; no implicit defaults remain in the runner.
 *
 * Everything but the session choices and the sign-in sources is the base layer
 * of the `tui` settings namespace, which the user's settings override.
 */
export interface RunnerOptions extends SessionOptions, AttachmentOptions {
  /**
   * Inline terminal scrollback or an application-owned alternate screen.
   * Set by `--screen` or the profile, it wins over the user's setting;
   * absent, the setting decides, and inline is the default.
   */
  readonly screen?: 'inline' | 'fullscreen' | null
  /** Profile's composer frame choice, or `auto` to read it from the terminal. */
  readonly composerFrame: FrameStyle | 'auto'
  readonly doubleInterruptMs: number
  /** Key references `/login` offers, with their labels and routes. */
  readonly credentialRefs: readonly CredentialTargetConfig[]
  /** Authorization flows `/login` offers, with the model each first sign-in starts on; absent offers every one. */
  readonly signInFlows?: readonly SignInFlowConfig[]
  readonly completionLimit: number
  /** Maximum tool-result preview lines; zero keeps only the headline and size. */
  readonly resultLines: number
  /** Whether the header names the goal's objective; off leaves it to the goal's sheet. */
  readonly goalObjective?: boolean
}

/** Wall-clock time and intervals for the turn header's animation and elapsed time. */
const systemClock: Clock = {
  now: () => performance.now(),
  every: (ms, tick) => {
    const timer = setInterval(tick, ms)
    return () => { clearInterval(timer) }
  },
}

/**
 * Ink's terminal suspension, which only a component can read. Handed to the
 * runner on mount and withdrawn on unmount.
 */
function TerminalOwner({ bind, children }: {
  readonly bind: (suspend: SuspendTerminal | undefined) => void
  readonly children?: React.ReactNode
}): React.ReactNode {
  const { suspendTerminal } = useApp()
  useEffect(() => {
    bind(callback => suspendTerminal(callback))
    return () => { bind(undefined) }
  }, [bind, suspendTerminal])
  return children
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
  if (io.in.isTTY !== true || io.out.isTTY !== true) {
    // Before the `try` below, which reports the failures it catches.
    const message = 'tui needs an interactive terminal; use dsh --profile headless for scripted runs'
    io.err.write(`dsh: ${message}\n`)
    throw new Error(message)
  }
  const abort = new AbortController()
  const done = Promise.withResolvers<void>()
  let App: AppComponent | undefined
  const uiReady = import(new URL('./ui-loader.js', import.meta.url).href).then(module => { App = module.App })
  let syntax: Syntax | undefined
  const syntaxReady = import(new URL('./syntax-loader.js', import.meta.url).href).then(({ createSyntax }) => { syntax = createSyntax() })
  // The highlighter module is deliberately separate from the runner. Its
  // Shiki graph can load while the profile loader and session open.
  const highlight: Highlight = (...args) => syntax?.highlight(...args)
  let ui: Instance | undefined
  // NO_COLOR suppresses text styling and animated indicators for this terminal.
  const motion = (process.env['NO_COLOR'] ?? '') === ''
  // Settled once the loader has, when the settings document has been read.
  let screen: 'inline' | 'fullscreen' = 'inline'
  let output: FrameOutput | undefined
  let navigation: SessionNavigation | undefined
  let quitTimer: ReturnType<typeof setTimeout> | undefined
  let terminalReleased = false
  // Set while the user's editor has the terminal. Rows committed meanwhile
  // wait for the redraw after it, since Ink discards frames while suspended.
  let editing = false
  let suspend: SuspendTerminal | undefined
  const bindSuspend = (next: SuspendTerminal | undefined): void => { suspend = next }
  const editText: EditText = async (text, name, signal) => {
    const owner = suspend
    if (owner === undefined || editing || terminalReleased) throw new Error('the terminal is not available')
    editing = true
    try {
      return await editExternally(callback => owner(async () => {
        // Ink's last writes before the handoff reach the terminal before the editor does.
        output?.flush()
        const release = holdInput(io.in)
        try { await callback() } finally { release() }
      }), text, name, signal)
    } finally {
      editing = false
      repaint()
    }
  }
  let completed = false
  let startupReport: Promise<void> | undefined
  let proxyModels: Promise<void> | undefined
  const releaseTerminal = (): void => {
    if (terminalReleased) return
    terminalReleased = true
    navigation?.close()
    clearTimeout(quitTimer)
    ui?.cleanup()
    // Restore the terminal now, before anything else writes to it.
    output?.flush()
  }
  // Settled only once `run`'s own `finally` has drained every owned background
  // task below. A root or fiber disposal invokes this disposer first and
  // awaits its return, so that path blocks on the same drains `finally` runs;
  // `finally`'s own `await stop()` comes after it resolves `drained`, so the
  // ordinary quit path never waits on itself.
  const drained = Promise.withResolvers<void>()
  const stop = ctx.effect(() => () => {
    abort.abort()
    releaseTerminal()
    done.resolve()
    return drained.promise
  }, 'tui terminal owner')
  const preferences = new Preferences(ctx, {
    screen: config.screen ?? 'inline', composerFrame: config.composerFrame,
    goalObjective: config.goalObjective ?? false, resultLines: config.resultLines,
    completionLimit: config.completionLimit, doubleInterruptMs: config.doubleInterruptMs, recentModels: [],
  }, config.screen ?? undefined, () => { repaint() }, editText)
  const copy: TuiCopy = dictionaries.en
  // Read once. The release does not change for the life of the process.
  const version = bakeVersion()
  const updates = new Updates({ running: version, release: releaseRoot() })
  // The status line's branch and changes, for whichever workspace is displayed.
  const git = new WorkspaceGit()
  // Resolved once per configured choice. The terminal it describes does not
  // change for the life of the process. The presentation layer takes the
  // result instead of reading the environment itself.
  let resolved: { readonly configured: FrameStyle | 'auto', readonly frame: FrameStyle } | undefined
  const frame = (): FrameStyle => {
    const configured = preferences.value.composerFrame
    if (resolved?.configured !== configured) {
      resolved = { configured, frame: resolveFrame({
        configured, env: process.env, platform: process.platform,
        systemLocale: () => Intl.DateTimeFormat().resolvedOptions().locale,
      }) }
    }
    return resolved.frame
  }
  // Runner state, not the controller's. The prompt outlives a session switch.
  // Routing it through `notify` put it in the same slot as command feedback,
  // where a command result cleared it and it cleared one in return.
  const interrupt = (): void => {
    if (quitTimer !== undefined) { done.resolve(); return }
    quitTimer = setTimeout(() => {
      quitTimer = undefined
      repaint()
    }, preferences.value.doubleInterruptMs)
    repaint()
  }
  const dismissQuit = (): void => {
    if (quitTimer === undefined) return
    clearTimeout(quitTimer)
    quitTimer = undefined
    repaint()
  }
  const element = (): React.ReactElement => {
    const View = App
    if (View === undefined) throw new Error('tui: UI module is not ready')
    const active = navigation?.controller
    if (navigation === undefined || active === undefined) throw new Error('tui: session is not connected')
    const settings = preferences.value
    const cwd = active.agent.session.header.cwd
    const branch = cwd === undefined ? undefined : git.follow(cwd, sessionGitConfinement(ctx, active.agent.session))
    return React.createElement(TerminalOwner, { bind: bindSuspend }, React.createElement(View, {
      ...active.view, key: active.agent.id, inputBlocked: navigation.busy, copy, frame: frame(), clock: systemClock, motion, screen,
      quitting: quitTimer !== undefined, completionLimit: settings.completionLimit, resultLines: settings.resultLines,
      goalObjective: settings.goalObjective,
      highlight, version, ...updates.state === undefined ? {} : { update: updates.state },
      ...updates.installing === undefined ? {} : { installing: updates.installing },
      // Shortened against home here: the presentation layer reads no environment.
      cwd: cwd === undefined ? '' : compactPath(cwd, process.env['HOME']), ...branch === undefined ? {} : { git: branch }, sessionId: active.agent.id,
      onReferenceQuery: query => active.references.search(query),
      onArgumentQuery: query => active.argumentQuery(query),
      onInspectSubagent: id => { navigation?.submit(`/agents ${id}`) },
      onCycleThinking: () => { active.cycleThinking() },
      onSubmit: text => navigation?.submit(text) ?? false, onCancel: () => navigation?.cancel(),
      onPasteImage: source => active.pasteImage(source), onRemoveImage: key => { active.removeImage(key) },
      onInterrupt: interrupt, onSendPending: () => { active.sendPending() }, onQuitDismiss: dismissQuit, onAnswer: (id, answer) => active.interactions.answer(id, answer),
    }))
  }
  const repaint = (): void => { if (!terminalReleased && !editing) ui?.rerender(element()) }
  try {
    await ctx.get('loader')?.await()
    abort.signal.throwIfAborted()
    // The screen holds for the life of the process; a change to it in
    // `/settings` is read at the next launch.
    screen = process.env['INK_SCREEN_READER'] === 'true' ? 'inline' : preferences.screen
    // One write per frame, drawn over the previous frame. Without synchronized
    // output, the controls would otherwise appear erased each time a line prints.
    output = frameOutput(io.out, io.err, motion, screen)
    // Before the session starts, so its first request already uses the
    // upgraded route. A route an earlier release wrote is brought up to what
    // the current login writes; one that cannot be is named for a new login.
    // Neither outcome may keep the terminal from opening.
    const routeNotice = cliProxyUpgradeNotice(await upgradeCliProxyRoute(ctx).catch((error: unknown) => {
        ctx.logger.warn('tui: CLIProxyAPI route upgrade failed: %o', error)
        return { kind: 'current' as const }
      }), copy)
    const login: LoginSources = { refs: config.credentialRefs, ...config.signInFlows === undefined ? {} : { flows: config.signInFlows } }
    navigation = new SessionNavigation(ctx, config, copy, login, repaint, updates, preferences)
    await Promise.all([uiReady, navigation.start(abort.signal)])
    abort.signal.throwIfAborted()
    // Before the first frame, so a known update is named from it; the network
    // request runs on behind it and never delays the session.
    updates.start(abort.signal, repaint)
    git.start(abort.signal, repaint)
    // Once per launch and behind the first frame: CLIProxyAPI's current list
    // replaces the one the login saved, so a model the proxy started serving
    // since then reaches `/model`. A proxy that cannot be read keeps the saved list.
    proxyModels = refreshCliProxyModels(ctx, abort.signal, cliProxyModelsInUse(ctx, navigation.controller?.model))
      .then(() => {}, (error: unknown) => {
        if (!abort.signal.aborted) ctx.logger.debug('tui: kept the saved CLIProxyAPI models: %o', error)
      })
    abort.signal.throwIfAborted()
    // Incremental rendering. A frame rewrites only the lines that changed.
    // A spinner tick or a streamed token repaints one row, not every control
    // row. That is the flicker on a terminal without synchronized output.
    ui = render(element(), {
      stdin: io.in, stdout: output.out, stderr: output.err, exitOnCtrlC: false, interactive: true, incrementalRendering: true,
      alternateScreen: screen === 'fullscreen',
    })
    const initial = navigation.controller!
    // A rejection the launcher survived goes on the notice line, since stderr
    // shares the screen. Once the terminal is released, the launcher's own
    // stderr line takes over.
    ctx.on('app/unhandled-rejection', ({ summary, record }) => {
      const active = navigation?.controller
      if (terminalReleased || active === undefined) return undefined
      const where = record === undefined ? ''
        : ` · ${copy.unhandledRejectionRecord} ${compactPath(record, process.env['HOME'])}`
      active.notify(`${copy.unhandledRejection}: ${summary} · ${copy.unhandledRejectionContinues}${where}`)
      return true
    })
    // Shown first, so a missing credential, which blocks every turn, replaces it.
    if (routeNotice !== undefined) initial.notify(routeNotice)
    startupReport = initial.reportCredentials().catch((error: unknown) => {
      initial.notify(error instanceof Error ? error.message : String(error))
    })
    await Promise.race([done.promise, ui.waitUntilExit()])
    completed = !abort.signal.aborted
  } catch (error) {
    if (!abort.signal.aborted) {
      // Reported immediately, before the drains below: a hung drain must not
      // swallow the message a caller needs to explain the exit.
      io.err.write(`dsh: ${error instanceof Error ? error.message : String(error)}\n`)
      throw error
    }
  } finally {
    // Abort before draining: `updates`/`git` background loops and navigation's
    // owned operations only stop once this signal fires, so it must land
    // before anything below awaits them, whether or not `stop()` already ran.
    abort.abort()
    releaseTerminal()
    try {
      await navigation?.drain()
      await startupReport
      await proxyModels
      await updates.drain()
      await git.drain()
      await syntaxReady
      await syntax?.close()
    } finally {
      // A failed drain must still release a disposal waiting on this gate.
      drained.resolve()
    }
    await stop()
  }
  if (completed) io.exit(0)
}
