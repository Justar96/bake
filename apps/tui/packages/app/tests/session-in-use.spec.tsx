/** A session another Bake process has open, refused through the real kernel write lock on every open path. */
import { EventEmitter } from 'node:events'
import { join } from 'node:path'
import React from 'react'
import { afterEach, expect, it, vi } from 'vitest'
import { Context } from '@deepseek-ai/cordis'
import { provideCmdline, SESSION_IN_USE_EXIT, SessionInUseError } from 'bake-cmdline'
import { createUserMessage } from 'bake-llm'
import type { SessionId } from 'bake-session'
import { SessionAlreadyOwnedError } from 'bake-session-persistence'
import Persistence from 'bake-session-persistence-jsonl'
import { App } from '@dsh-tui/ui/app.tsx'
import { dictionaries } from '@dsh-tui/ui/copy.ts'
import { cleanup as unmount, render } from '../../../tests/render.tsx'
import { apply, type Config } from '../src/index.ts'
import { SessionNavigation } from '../src/navigation.ts'
import { run, type TuiIo } from '../src/runner.ts'
import { openSession } from '../src/session.ts'
import { harness } from './harness.ts'

const cleanup: (() => Promise<void>)[] = []
afterEach(async () => {
  unmount()
  for (const dispose of cleanup.splice(0).reverse()) await dispose()
})

const options = { attachmentMaxBytes: 1048576, attachmentLimit: 8 }
const launch = {
  composerFrame: 'auto', completionLimit: 8, resultLines: 8, doubleInterruptMs: 500, credentialRefs: [], ...options,
} as const

class Input extends EventEmitter {
  isTTY = true
  isRaw = false
  setEncoding() {}
  setRawMode(value: boolean) { this.isRaw = value }
  resume() {}
  pause() {}
  ref() {}
  unref() {}
  read() { return null }
}
class Output extends EventEmitter {
  isTTY = true
  columns = 100
  rows = 30
  frames: string[] = []
  write(chunk: string, callback?: () => void) { this.frames.push(chunk); callback?.(); return true }
}

/** @returns services with one saved session in the current workspace, and that session's id. */
async function withSaved() {
  const fixture = await harness()
  cleanup.push(fixture.dispose)
  const saved = await openSession(fixture.ctx, {}, new AbortController().signal, () => {})
  saved.agent.followup(createUserMessage({ content: [{ type: 'text', text: 'Saved' }], source: { kind: 'user' } }))
  await saved.agent.whenIdle()
  await saved.dispose()
  return { ...fixture, id: saved.agent.id }
}

/**
 * Take the session's write lock through a second store on the same directory,
 * as another Bake process does: its own persistence instance, so the refusal
 * comes from the kernel lock and not from this process's write claim.
 * @returns the release, which closes the handle and the store.
 */
async function holdElsewhere(root: string, id: SessionId): Promise<() => Promise<void>> {
  const other = new Context()
  await other.plugin(Persistence, { root: join(root, 'sessions'), compression: 'none' })
  const handle = await other.sessionPersistence.open(id, 'write')
  let released = false
  const release = async () => {
    if (released) return
    released = true
    await handle.close()
    await other.fiber.dispose()
  }
  cleanup.push(release)
  return release
}

it.each(['en'] as const)(
  '/resume keeps the current session and says another process has the chosen one open (%s)', async locale => {
    const copy = dictionaries[locale]
    const { ctx, root, id, model } = await withSaved()
    const release = await holdElsewhere(root, id)
    const navigation = new SessionNavigation(ctx, options, copy, { refs: [] }, vi.fn())
    cleanup.push(async () => { navigation.close(); await navigation.drain() })
    await navigation.start(new AbortController().signal)
    const current = navigation.controller!
    const choose = async (): Promise<void> => {
      navigation.submit('/resume')
      await vi.waitFor(() => expect(navigation.controller!.view.interaction?.kind).toBe('select'))
      const prompt = navigation.controller!.view.interaction!
      // Listed: the lock is only tried when a session is chosen.
      if (prompt.kind !== 'select' || !prompt.choices.some(choice => choice.value === id)) throw new Error('Expected the saved session')
      navigation.controller!.interactions.answer(prompt.id, id)
      await vi.waitFor(() => expect(navigation.busy).toBe(false))
    }
    await choose()
    expect(navigation.controller).toBe(current)
    expect(current.view.notice).toBe(copy.sessionInUse)
    expect(ctx.agents.get(id)).toBeUndefined()
    expect((await ctx.sessionQuery.listSessions()).filter(record => record.live).map(record => record.header.id)).toEqual([current.agent.id])
    expect(model.requests).toHaveLength(1)
    // The runner's own frame, over this controller's view; the id and workspace are fixed for the recording.
    const ui = render(<App {...current.view} copy={copy} frame="round" quitting={false} completionLimit={8} resultLines={4}
      cwd="/workspace" sessionId="session-current" onReferenceQuery={() => {}}
      onSubmit={() => false} onCancel={() => {}} onInterrupt={() => {}} onAnswer={() => {}} />)
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('/new'))
    await expect(ui.lastFrame() + '\n').toMatchFileSnapshot(`./expected/session-in-use.${locale}.txt`)
    // Once that process lets the session go, the same choice opens it: the refused open kept no claim.
    await release()
    await choose()
    expect(navigation.controller!.agent.id).toBe(id)
  })

it('refuses --resume before the first frame with a localized reason, leaving the terminal untouched', async () => {
  const { ctx, root, id } = await withSaved()
  await holdElsewhere(root, id)
  const input = new Input()
  const output = new Output()
  const error = new Output()
  const exit = vi.fn()
  const thrown: unknown = await run(ctx, { ...launch, resume: id }, { in: input, out: output, err: error, exit } as unknown as TuiIo)
    .then(() => undefined, (reason: unknown) => reason)
  expect(thrown).toBeInstanceOf(SessionInUseError)
  expect((thrown as Error).message).toBe(`${id}: ${dictionaries.en.sessionInUseLaunch}`)
  expect((thrown as Error).cause).toBeInstanceOf(SessionAlreadyOwnedError)
  expect(input.isRaw).toBe(false)
  expect(input.listenerCount('readable')).toBe(0)
  expect(output.frames).toEqual([])
  // Reported immediately when caught, not deferred behind the caller's own catch.
  expect(error.frames.join('')).toBe(`dsh: ${id}: ${dictionaries.en.sessionInUseLaunch}\n`)
  expect(exit).not.toHaveBeenCalled()
  expect(ctx.agents.get(id)).toBeUndefined()
})

it('prints one stderr line and requests the in-use exit status through the plugin entry', async () => {
  const { ctx, root, id } = await withSaved()
  await holdElsewhere(root, id)
  const exited = Promise.withResolvers<number>()
  provideCmdline(ctx, { args: [], exit: code => { exited.resolve(code) } })
  // The entry binds the process's own streams. The refusal comes before Ink
  // mounts, so they are only asked whether they are terminals.
  for (const stream of [process.stdin, process.stdout]) {
    const own = Object.getOwnPropertyDescriptor(stream, 'isTTY')
    Object.defineProperty(stream, 'isTTY', { value: true, configurable: true })
    cleanup.push(async () => {
      if (own === undefined) Reflect.deleteProperty(stream, 'isTTY')
      else Object.defineProperty(stream, 'isTTY', own)
    })
  }
  const stderr = vi.spyOn(process.stderr, 'write').mockImplementation(() => true)
  cleanup.push(async () => { stderr.mockRestore() })
  const stdout = vi.spyOn(process.stdout, 'write')
  cleanup.push(async () => { stdout.mockRestore() })
  apply(ctx, { ...launch, credentialRefs: [], resume: id } satisfies Config)
  expect(await exited.promise).toBe(SESSION_IN_USE_EXIT)
  expect(stderr.mock.calls).toEqual([[`dsh: ${id}: ${dictionaries.en.sessionInUseLaunch}\n`]])
  expect(stdout).not.toHaveBeenCalled()
})
