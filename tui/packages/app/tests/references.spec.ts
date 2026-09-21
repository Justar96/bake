/** File discovery delegates cwd and cancellation to Harness without reading content into prompts. */
import { mkdir, writeFile } from 'node:fs/promises'
import { join } from 'node:path'
import { afterEach, expect, it, vi } from 'vitest'
import LocalFileReferences from '@deepseek-ai/dsh-file-reference-local'
import type { FileReferenceCandidate } from '@deepseek-ai/dsh-file-reference/types'
import { SessionId } from '@deepseek-ai/dsh-session'
import { dictionaries } from '@dsh-tui/ui/copy.ts'
import { SessionController } from '../src/controller.ts'
import { harness } from './harness.ts'

const cleanup: (() => Promise<void>)[] = []
afterEach(async () => { for (const dispose of cleanup.splice(0).reverse()) await dispose() })

async function connected(provider = true) {
  const fixture = await harness()
  cleanup.push(fixture.dispose)
  if (provider) await fixture.ctx.plugin(LocalFileReferences)
  const workspace = join(fixture.root, 'workspace')
  await mkdir(join(workspace, 'notes folder'), { recursive: true })
  await writeFile(join(workspace, 'notes folder', 'read me.txt'), 'FILE_CONTENT_MUST_NOT_BE_INJECTED')
  await writeFile(join(fixture.root, 'outside.txt'), 'Outside this session')
  let controller!: SessionController
  const changed = vi.fn()
  const handle = await fixture.ctx.agents.create({
    sessionId: SessionId('file-reference-session'), meta: { cwd: workspace }, agentOptions: { provider: 'mock', model: 'model' },
    setup: (_ctx, agent) => { controller = new SessionController(fixture.ctx, agent, dictionaries.en, [], changed, { attachmentMaxBytes: 1048576, attachmentLimit: 8 }) },
  })
  cleanup.push(async () => { controller.close(); await controller.drain(); await handle.dispose() })
  await controller.replay(new AbortController().signal)
  return { ...fixture, handle, controller, changed }
}

it('discovers paths inside the session cwd and submits only the literal mention', async () => {
  const { controller, handle, model } = await connected()
  const before = handle.agent.session.snapshotEvents()
  controller.references.search('')
  await controller.drain()
  expect(controller.view.files.entries).toEqual([{ path: 'notes folder', kind: 'directory' }])
  controller.references.search('notes folder/')
  await controller.drain()
  expect(controller.view.files.entries).toEqual([{ path: 'notes folder/read me.txt', kind: 'file' }])
  expect(model.requests).toHaveLength(0)
  expect(handle.agent.session.snapshotEvents()).toEqual(before)
  controller.submit('Review @"notes folder/read me.txt" ')
  await handle.agent.whenIdle()
  expect(model.requests).toHaveLength(1)
  expect(JSON.stringify(model.requests)).not.toContain('FILE_CONTENT_MUST_NOT_BE_INJECTED')
  const inputs = handle.agent.session.snapshotEvents().flatMap(event => event.type === 'user/message'
    ? [{ source: event.data.source, content: event.data.content }] : [])
  await expect(JSON.stringify(inputs, null, 2) + '\n').toMatchFileSnapshot('./expected/file-reference.json')
})

it('aborts superseded queries and prevents late results from replacing current paths', async () => {
  const { ctx, controller, handle } = await connected()
  const started = Promise.withResolvers<AbortSignal>()
  const stale = Promise.withResolvers<FileReferenceCandidate[]>()
  const list = vi.spyOn(ctx.fileReferences, 'list').mockImplementation(async (agent, query, signal) => {
    expect(agent).toBe(handle.agent)
    if (query === 'old') { started.resolve(signal); return stale.promise }
    return [{ path: 'current.txt', kind: 'file' }]
  })
  try {
    controller.references.search('old')
    const signal = await started.promise
    controller.references.search('new')
    await vi.waitFor(() => expect(controller.view.files.loading).toBe(false))
    expect(signal.aborted).toBe(true)
    stale.resolve([{ path: 'old.txt', kind: 'file' }])
    await controller.drain()
    expect(controller.view.files.entries).toEqual([{ path: 'current.txt', kind: 'file' }])
    controller.references.search('new')
    expect(list).toHaveBeenCalledTimes(2)
    controller.references.search(undefined)
    expect(controller.view.files.entries).toEqual([])
  } finally { stale.resolve([]) }
})

it('aborts on dismissal and drains an outstanding read without notifying after closure', async () => {
  const { ctx, controller, changed } = await connected()
  const started = Promise.withResolvers<AbortSignal>()
  const late = Promise.withResolvers<FileReferenceCandidate[]>()
  vi.spyOn(ctx.fileReferences, 'list').mockImplementation(async (_agent, _query, signal) => { started.resolve(signal); return late.promise })
  try {
    controller.references.search('pending')
    const signal = await started.promise
    controller.references.search(undefined)
    expect(signal.aborted).toBe(true)
    controller.close()
    const notifications = changed.mock.calls.length
    let drained = false
    const draining = controller.drain().then(() => { drained = true })
    await Promise.resolve()
    expect(drained).toBe(false)
    late.resolve([{ path: 'late.txt', kind: 'file' }])
    await draining
    controller.references.search('closed')
    expect(changed).toHaveBeenCalledTimes(notifications)
    expect(controller.view.files.entries).toEqual([])
  } finally { late.resolve([]) }
})

it('reports provider errors and aborts an active search at shutdown', async () => {
  const { ctx, controller } = await connected()
  const list = vi.spyOn(ctx.fileReferences, 'list').mockRejectedValueOnce(new Error('Workspace unavailable'))
  controller.references.search('broken')
  await controller.drain()
  expect(controller.view.files).toMatchObject({ loading: false, error: 'Workspace unavailable' })
  const started = Promise.withResolvers<AbortSignal>()
  list.mockImplementation(async (_agent, _query, signal) => {
    started.resolve(signal)
    await new Promise<void>((_resolve, reject) => signal.addEventListener('abort', () => { reject(signal.reason) }, { once: true }))
    return []
  })
  controller.references.search('pending')
  const signal = await started.promise
  controller.close()
  await controller.drain()
  expect(signal.aborted).toBe(true)
})

it('reports a missing provider and keeps literal reference submission available', async () => {
  const { controller, handle, model } = await connected(false)
  controller.references.search('manual.txt')
  await controller.drain()
  expect(controller.view.files.error).toBe(dictionaries.en.filesUnavailable)
  controller.submit('@manual.txt')
  await handle.agent.whenIdle()
  expect(model.requests).toHaveLength(1)
})
