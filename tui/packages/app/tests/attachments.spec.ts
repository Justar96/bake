/** Attachment drafts, cancellation, and durable replay through real Harness services. */
import { readFile, writeFile } from 'node:fs/promises'
import { join } from 'node:path'
import { afterEach, expect, it, vi } from 'vitest'
import Attachments, { type Config as StoreConfig } from '@deepseek-ai/dsh-attachment-local'
import FileSystem from '@deepseek-ai/dsh-fs-local'
import { formatRow, transcriptRows } from '@dsh-tui/ui'
import { dictionaries } from '@dsh-tui/ui/copy.ts'
import type { AttachmentOptions } from '../src/attachments.ts'
import { SessionNavigation } from '../src/navigation.ts'
import { harness, textResponse } from './harness.ts'

const copy = dictionaries.en
const cleanup: (() => Promise<void> | void)[] = []
afterEach(async () => { for (const dispose of cleanup.splice(0).reverse()) await dispose() })
const png = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAACXBIWXMAAAPoAAAD6AG1e1JrAAAADElEQVQImWNgZGIGAAAOAAeCcsnOAAAAAElFTkSuQmCC', 'base64')

function barrier() {
  const entered = Promise.withResolvers<void>()
  const release = Promise.withResolvers<void>()
  cleanup.push(() => release.resolve())
  return { entered: entered.promise, release: release.resolve, wait: async () => { entered.resolve(); await release.promise } }
}

async function connected(limits: Partial<AttachmentOptions> = {}, store: StoreConfig = {}) {
  const fixture = await harness()
  cleanup.push(fixture.dispose)
  await fixture.ctx.plugin(FileSystem, { cwd: fixture.root })
  await fixture.ctx.plugin(Attachments, { dshHome: fixture.root, ...store })
  await writeFile(join(fixture.root, 'notes with spaces.bin'), Buffer.from([0, 1, 2, 255]))
  await writeFile(join(fixture.root, 'pixel.png'), png)
  const options = { attachmentMaxBytes: 1024, attachmentLimit: 8, ...limits }
  const navigation = new SessionNavigation(fixture.ctx, options, copy, [], () => {})
  cleanup.push(async () => { navigation.close(); await navigation.drain() })
  await navigation.start(new AbortController().signal)
  const controller = navigation.controller!
  const stage = async (path = 'notes with spaces.bin') => {
    expect(navigation.submit(`/attach ${path}`)).toBe(true)
    await controller.drain()
  }
  return { ...fixture, options, navigation, controller, stage }
}

it('stages literal paths, removes and clears drafts without storing or sending bytes', async () => {
  const { ctx, controller, stage, model } = await connected()
  const save = vi.spyOn(ctx.attachments, 'saveFile')
  await stage()
  expect(controller.view.attachments).toEqual([{ name: 'notes with spaces.bin', bytes: 4 }])
  await stage('pixel.png')
  expect(controller.view.attachments).toHaveLength(2)
  controller.submit('/remove-attachment 0')
  await controller.drain()
  expect(controller.view.attachments).toHaveLength(2)
  controller.submit('/remove-attachment 1')
  await controller.drain()
  expect(controller.view.attachments[0]?.name).toBe('pixel.png')
  controller.submit('/clear-attachments')
  await controller.drain()
  expect(controller.view.attachments).toEqual([])
  expect(save).not.toHaveBeenCalled()
  expect(model.requests).toEqual([])
})

it('retains the draft when source byte/count limits or image validation refuse a file', async () => {
  const { controller, stage, root } = await connected({ attachmentLimit: 1, attachmentMaxBytes: 4 })
  await stage('pixel.png')
  expect(controller.view.attachments).toEqual([])
  await stage()
  await stage()
  expect(controller.view.attachments).toHaveLength(1)
  expect(transcriptRows(controller.view.committed).at(-1)).toMatchObject({ kind: 'notice', tone: 'error', text: copy.attachmentCountLimit })
  controller.submit('/clear-attachments')
  await controller.drain()
  await writeFile(join(root, 'bad.png'), 'bad')
  await stage('bad.png')
  expect(controller.view.attachments).toEqual([])
  await stage('missing')
  expect(controller.view.attachments).toEqual([])
})

it('logs file and image references in order and resumes their metadata and exact stored bytes', async () => {
  const { ctx, controller, navigation, options, stage, model } = await connected()
  vi.spyOn(model, 'resolveModel').mockImplementation(async (provider, id) => ({ provider, id, name: id, inputModalities: ['text', 'image'] }))
  await stage()
  await stage('pixel.png')
  expect(await navigation.submit('Inspect these')).toBe(true)
  expect(controller.view.attachments).toEqual([])
  await controller.agent.whenIdle()
  using before = await ctx.sessionQuery.observeSession(controller.agent.id, { projectionMode: 'none' })
  const message = before.events.find(event => event.type === 'user/message' && event.data.source.kind === 'user')
  if (message?.type !== 'user/message') throw new Error('Missing user message')
  expect(message.data.content.map(block => block.type)).toEqual(['text', 'file', 'image'])
  const file = message.data.content[1]!
  const image = message.data.content[2]!
  if (file.type !== 'file' || image.type !== 'image') throw new Error('Missing attachments')
  expect(await readFile(ctx.attachments.fileHostPath(file.attachment)!)).toEqual(Buffer.from([0, 1, 2, 255]))
  expect(Buffer.from((await ctx.attachments.readImage(image.attachment)).data)).toEqual(png)
  const rows = transcriptRows(controller.view.committed)
  await expect(rows.map(formatRow).join('\n') + '\n').toMatchFileSnapshot('./expected/attachments.txt')
  const id = controller.agent.id
  navigation.close()
  await navigation.drain()
  const resumed = new SessionNavigation(ctx, { ...options, resume: id }, copy, [], () => {})
  cleanup.push(async () => { resumed.close(); await resumed.drain() })
  await resumed.start(new AbortController().signal)
  expect(transcriptRows(resumed.controller!.view.committed)).toEqual(rows)
  expect(resumed.controller!.view.attachments).toEqual([])
  expect(model.requests).toHaveLength(1)
})

it('retains staged images on a text-only model and on Harness aggregate-image admission failure', async () => {
  const { controller, stage, ctx, model } = await connected({}, { maxImagesPerMessage: 1 })
  await stage('pixel.png')
  const save = vi.spyOn(ctx.attachments, 'saveImages')
  expect(await controller.submit('Look')).toBe(false)
  expect(controller.view.notice).toBe(copy.modelNoImages)
  expect(save).not.toHaveBeenCalled()
  vi.spyOn(model, 'resolveModel').mockImplementation(async (provider, id) => ({ provider, id, name: id, inputModalities: ['image'] }))
  await stage('pixel.png')
  expect(await controller.submit('Look')).toBe(false)
  expect(save).toHaveBeenCalledWith(expect.arrayContaining([expect.objectContaining({ mediaType: 'image/png' })]))
  expect(save.mock.calls[0]![0]).toHaveLength(2)
  expect(controller.view.attachments).toHaveLength(2)
  expect(model.requests).toEqual([])
})

it('rejects unsupported commands and navigation while attachments remain staged', async () => {
  const { ctx, navigation, controller, stage } = await connected()
  const command = vi.fn(() => ({ kind: 'success' as const }))
  cleanup.push(controller.agent.ctx.effect(() => ctx.commands.register({ name: 'external', description: 'External', handler: command })))
  await stage()
  expect(controller.submit('/external')).toBe(false)
  expect(controller.view.notice).toBe(copy.attachmentCommandsUnsupported)
  expect(command).not.toHaveBeenCalled()
  navigation.submit('/sessions')
  await controller.drain()
  expect(controller.view.interaction).toBeUndefined()
  await vi.waitFor(() => expect(navigation.busy).toBe(false))
  expect(controller.view.notice).toContain(copy.attachmentsBeforeNavigation)
  expect(controller.view.attachments).toHaveLength(1)
})

it('cancels a source read and drains its late result without staging it', async () => {
  const { ctx, controller } = await connected()
  const gate = barrier()
  vi.spyOn(ctx.fs, 'readBytes').mockImplementation(async () => { await gate.wait(); return png })
  controller.submit('/attach pixel.png')
  await gate.entered
  expect(controller.submit('Wait for the file')).toBe(false)
  controller.cancel()
  let drained = false
  const done = controller.drain().then(() => { drained = true })
  expect(drained).toBe(false)
  gate.release()
  await done
  expect(controller.view.attachments).toEqual([])
})

it.each(['cancel', 'close'] as const)('drains non-abortable storage on %s and never admits its late result', async action => {
  const { ctx, controller, stage, model } = await connected()
  await stage()
  const gate = barrier()
  const save = ctx.attachments.saveFile.bind(ctx.attachments)
  vi.spyOn(ctx.attachments, 'saveFile').mockImplementation(async input => { await gate.wait(); return save(input) })
  const accepted = controller.submit('Retain this text')
  await gate.entered
  expect(controller.submit('/clear-attachments')).toBe(false)
  if (action === 'cancel') controller.cancel()
  else controller.close()
  let drained = false
  const done = controller.drain().then(() => { drained = true })
  expect(drained).toBe(false)
  gate.release()
  expect(await accepted).toBe(false)
  await done
  expect(model.requests).toEqual([])
  expect(controller.agent.inbox.nextTurn).toEqual([])
  expect(controller.view.attachments).toHaveLength(action === 'cancel' ? 1 : 0)
})

it('cancels model lookup before storing images and retains them for retry', async () => {
  const { controller, ctx, model, stage } = await connected()
  await stage('pixel.png')
  const gate = barrier()
  vi.spyOn(model, 'resolveModel').mockImplementation(async (provider, id) => { await gate.wait(); return { provider, id, name: id, inputModalities: ['image'] } })
  const save = vi.spyOn(ctx.attachments, 'saveImages')
  const accepted = controller.submit('Look')
  await gate.entered
  controller.cancel()
  gate.release()
  expect(await accepted).toBe(false)
  expect(save).not.toHaveBeenCalled()
  expect(controller.view.attachments).toHaveLength(1)
})

it('retains sources after a storage failure, then accepts an attachment-only prompt', async () => {
  const { ctx, controller, stage } = await connected()
  await stage()
  vi.spyOn(ctx.attachments, 'saveFile').mockRejectedValueOnce(new Error('Storage unavailable'))
  expect(await controller.submit('')).toBe(false)
  expect(controller.view.notice).toBe('Storage unavailable')
  expect(controller.view.attachments).toHaveLength(1)
  expect(await controller.submit('')).toBe(true)
  await controller.agent.whenIdle()
  expect(transcriptRows(controller.view.committed)).toContainEqual({ kind: 'user', text: '', attachments: [{ name: 'notes with spaces.bin', bytes: 4 }] })
})

it('reads Agent activity at admission time and shows queued attachment metadata', async () => {
  const { ctx, controller, model, stage } = await connected()
  await stage()
  const saving = barrier()
  const streaming = barrier()
  const save = ctx.attachments.saveFile.bind(ctx.attachments)
  vi.spyOn(ctx.attachments, 'saveFile').mockImplementation(async input => { await saving.wait(); return save(input) })
  const accepted = controller.submit('Next step')
  await saving.entered
  model.response = async function* () { await streaming.wait(); yield* textResponse('Done') }
  const followup = vi.spyOn(controller.agent, 'followup')
  const steer = vi.spyOn(controller.agent, 'steer')
  const { createUserMessage } = await import('@deepseek-ai/dsh-llm')
  controller.agent.followup(createUserMessage({ content: [{ type: 'text', text: 'Start' }], source: { kind: 'user' } }))
  await streaming.entered
  saving.release()
  expect(await accepted).toBe(true)
  expect(followup).toHaveBeenCalledTimes(1)
  expect(steer).toHaveBeenCalledTimes(1)
  expect(controller.view.pending).toContainEqual(expect.objectContaining({ target: 'next-step', text: 'Next step', attachments: [{ name: 'notes with spaces.bin', bytes: 4 }] }))
  controller.agent.cancel({ kind: 'user' }, { keepInbox: true })
  streaming.release()
  await controller.agent.whenIdle()
})

it('bounds the aggregate staged source bytes and the Harness image batch bytes independently', async () => {
  const files = await connected({ attachmentMaxBytes: 7 })
  await files.stage()
  await files.stage()
  expect(files.controller.view.attachments).toHaveLength(1)
  expect(transcriptRows(files.controller.view.committed).at(-1)).toMatchObject({ kind: 'notice', tone: 'error' })
  const images = await connected({}, { maxMessageImageBytes: png.length })
  vi.spyOn(images.model, 'resolveModel').mockImplementation(async (provider, id) => ({ provider, id, name: id, inputModalities: ['image'] }))
  await images.stage('pixel.png')
  await images.stage('pixel.png')
  expect(await images.controller.submit('Look')).toBe(false)
  expect(images.controller.view.attachments).toHaveLength(2)
  expect(images.model.requests).toEqual([])
})

it('retains the draft if the Agent refuses inbox acceptance after storage', async () => {
  const { controller, stage } = await connected()
  await stage()
  vi.spyOn(controller.agent, 'followup').mockImplementationOnce(() => { throw new Error('Inbox unavailable') })
  expect(await controller.submit('Retry me')).toBe(false)
  expect(controller.view.notice).toBe('Inbox unavailable')
  expect(controller.view.attachments).toHaveLength(1)
  expect(controller.agent.inbox.nextTurn).toEqual([])
})

it('refuses late admission when another Harness consumer changes the selected model', async () => {
  const { controller, ctx, stage, model } = await connected()
  vi.spyOn(model, 'resolveModel').mockImplementation(async (provider, id) => ({ provider, id, name: id, inputModalities: ['image'] }))
  await stage('pixel.png')
  const gate = barrier()
  const save = ctx.attachments.saveImages.bind(ctx.attachments)
  vi.spyOn(ctx.attachments, 'saveImages').mockImplementation(async input => { await gate.wait(); return save(input) })
  const accepted = controller.submit('Look')
  await gate.entered
  await ctx.commands.execute(controller.agent, '/model mock/other', [], new AbortController().signal)
  gate.release()
  expect(await accepted).toBe(false)
  expect(controller.view.notice).toBe(copy.attachmentModelChanged)
  expect(controller.view.attachments).toHaveLength(1)
  expect(model.requests).toEqual([])
})
