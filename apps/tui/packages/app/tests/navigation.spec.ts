/** Session handoff, rollback, and cancellation through real Harness lifecycle services. */
import { afterEach, expect, it, vi } from 'vitest'
import { brandString } from '@deepseek-ai/dsh-brand'
import { CommandId } from '@deepseek-ai/dsh-commands'
import type { SessionId } from '@deepseek-ai/dsh-session'
import { createUserMessage } from '@deepseek-ai/dsh-llm'
import { transcriptRows } from '@dsh-tui/ui'
import { dictionaries } from '@dsh-tui/ui/copy.ts'
import { SessionNavigation } from '../src/navigation.ts'
import { openSession } from '../src/session.ts'
import { harness } from './harness.ts'

const cleanup: (() => Promise<void>)[] = []
afterEach(async () => { for (const dispose of cleanup.splice(0).reverse()) await dispose() })
const copy = dictionaries.en
const user = (text: string) => createUserMessage({ content: [{ type: 'text' as const, text }], source: { kind: 'user' as const } })

async function connected() {
  const fixture = await harness()
  cleanup.push(fixture.dispose)
  const changed = vi.fn()
  const navigation = new SessionNavigation(fixture.ctx, { attachmentMaxBytes: 1048576, attachmentLimit: 8 }, copy, { refs: [] }, changed)
  cleanup.push(async () => { navigation.close(); await navigation.drain() })
  await navigation.start(new AbortController().signal)
  return { ...fixture, navigation, changed }
}

async function picker(navigation: SessionNavigation, command: '/sessions' | '/resume' = '/sessions') {
  navigation.submit(command)
  await vi.waitFor(() => expect(navigation.controller?.view.interaction?.kind).toBe('select'))
  const interaction = navigation.controller!.view.interaction!
  if (interaction.kind !== 'select') throw new Error('Expected session picker')
  return interaction
}

it.each(['/new', '/clear'] as const)('%s starts a fresh session without opening the picker', async command => {
  const { ctx, navigation } = await connected()
  const first = navigation.controller!
  navigation.submit(command)
  await vi.waitFor(() => expect(navigation.controller).not.toBe(first))
  await vi.waitFor(() => expect(navigation.busy).toBe(false))
  expect(navigation.controller?.view.interaction).toBeUndefined()
  expect(ctx.agents.get(first.agent.id)).toBeUndefined()
  expect(navigation.controller?.view.committed.length).toBe(0)
})

async function select(navigation: SessionNavigation, value: string) {
  const prompt = await picker(navigation)
  navigation.controller!.interactions.answer(prompt.id, value)
  await vi.waitFor(() => expect(navigation.busy).toBe(false))
}

it('creates, switches, and resumes recorded history with one live handle and session-owned commands', async () => {
  const { ctx, navigation, model } = await connected()
  const first = navigation.controller!
  navigation.submit('/model mock/special')
  await first.drain()
  navigation.submit('First conversation')
  await first.agent.whenIdle()
  first.agent.session.append('session/title', { title: 'First title', messageSeqs: [], source: { kind: 'user' } })
  await select(navigation, '')
  const second = navigation.controller!
  expect(second.agent.id).not.toBe(first.agent.id)
  expect(ctx.agents.get(first.agent.id)).toBeUndefined()
  expect(ctx.sessions.get(first.agent.id)).toBeUndefined()
  expect(transcriptRows(second.view.committed)).toEqual([])
  expect(second.view.pending).toEqual([])
  expect(second.view.context).toBeUndefined()
  expect(second.view.model).toBe('mock/model')
  navigation.submit('Second conversation')
  await second.agent.whenIdle()
  const firstBefore = first.view.committed
  const prompt = await picker(navigation, '/resume')
  expect(prompt.choices).toContainEqual(expect.objectContaining({ value: first.agent.id, label: 'First title' }))
  expect(prompt.title).toBe(copy.chooseSession)
  expect(prompt.choices.at(-1)).toMatchObject({ value: '', label: copy.newSession, pinned: true })
  expect(prompt.choices.map(choice => choice.role)).toEqual(['session-current', 'session-saved', 'session-new'])
  expect(prompt.choices[0]?.value).toBe(second.agent.id)
  expect(prompt.choices[1]?.value).toBe(first.agent.id)
  const requests = model.requests.length
  second.interactions.answer(prompt.id, first.agent.id)
  await vi.waitFor(() => expect(navigation.busy).toBe(false))
  const resumed = navigation.controller!
  expect(resumed.agent.id).toBe(first.agent.id)
  expect(resumed.view.model).toBe('mock/special')
  expect(first.view.committed).toBe(firstBefore)
  expect(ctx.agents.get(second.agent.id)).toBeUndefined()
  expect(transcriptRows(resumed.view.committed)).toContainEqual({ kind: 'user', text: 'First conversation' })
  expect(transcriptRows(resumed.view.committed)).not.toContainEqual({ kind: 'user', text: 'Second conversation' })
  expect(resumed.view.completion.entries.filter(entry => entry.name === 'sessions')).toHaveLength(1)
  expect(resumed.view.completion.entries.filter(entry => entry.name === 'resume')).toHaveLength(1)
  expect(model.requests).toHaveLength(requests)
  using observation = await ctx.sessionQuery.observeSession(second.agent.id, { projectionMode: 'none' })
  const runs = observation.events.filter(event => event.type === 'command/run')
  const done = observation.events.filter(event => event.type === 'command/done')
  expect(runs).toHaveLength(1)
  expect(done).toHaveLength(1)
  expect(done[0]!.data.commandId).toBe(runs[0]!.data.commandId)
  await select(navigation, second.agent.id)
  expect(navigation.controller!.agent.id).toBe(second.agent.id)
})

it('keeps the current controller on dismissal or current selection and rejects command arguments', async () => {
  const { navigation, model } = await connected()
  const first = navigation.controller!
  await picker(navigation)
  navigation.cancel()
  await vi.waitFor(() => expect(navigation.busy).toBe(false))
  expect(navigation.controller).toBe(first)
  expect(first.view.notice).toBe(copy.sessionsCancelled)
  await select(navigation, first.agent.id)
  expect(navigation.controller).toBe(first)
  navigation.submit('/sessions invalid')
  await first.drain()
  expect(transcriptRows(first.view.committed).at(-1)).toEqual({ kind: 'notice', placement: 'command', tone: 'error', text: `${copy.usage}: /sessions` })
  navigation.submit('/resume invalid')
  await first.drain()
  expect(transcriptRows(first.view.committed).at(-1)).toEqual({ kind: 'notice', placement: 'command', tone: 'error', text: `${copy.usage}: /resume` })
  navigation.submit('/new invalid')
  await first.drain()
  expect(transcriptRows(first.view.committed).at(-1)).toEqual({ kind: 'notice', placement: 'command', tone: 'error', text: `${copy.usage}: /new` })
  expect(model.requests).toEqual([])
})

it('shows a localized, actionable notice for a session recorded before agentPresets existed', async () => {
  const { ctx, navigation } = await connected()
  const first = navigation.controller!
  const legacy = await ctx.agents.create({
    sessionId: brandString<SessionId>('legacy'), meta: { cwd: process.cwd() }, agentOptions: { provider: 'mock', model: 'model' },
  })
  legacy.agent.followup(user('Persist'))
  await legacy.agent.whenIdle()
  await legacy.dispose()
  await select(navigation, 'legacy')
  // Rolled back to the still-current session, same as any other open failure,
  // with the localized notice rather than `openSession`'s raw error text.
  expect(navigation.controller).toBe(first)
  expect(first.view.notice).toBe(copy.sessionNeedsPreset)
})

it('pins the active session before saved history ordered by last use, pointing at the newest', async () => {
  const { ctx, navigation } = await connected()
  const active = navigation.controller!.agent.id
  for (const id of ['older', 'newer', 'blank', 'opened']) {
    const handle = await ctx.agents.create({
      sessionId: brandString<SessionId>(`session-${id}`),
      meta: { cwd: process.cwd() },
      agentOptions: { provider: 'mock', model: 'model' },
    })
    if (id === 'opened') {
      // Opened, one command run, and left: no turn, so the picker leaves it out.
      const commandId = CommandId('opened-model')
      handle.agent.session.append('command/run', { commandId, name: 'model', source: { kind: 'user' } })
      handle.agent.session.append('command/done', { commandId, kind: 'success' })
    } else {
      // A real turn, so the session persists and counts as used; only the blank one stays untitled.
      handle.agent.followup(user(id === 'blank' ? 'No title yet' : `About ${id}`))
      await handle.agent.whenIdle()
      if (id !== 'blank') handle.agent.session.append('session/title', { title: id, messageSeqs: [], source: { kind: 'user' } })
    }
    await handle.dispose()
  }
  // Created older, blank, newer; used older last, so it leads despite its age.
  const created: Record<string, number> = { 'session-older': 100, 'session-blank': 150, 'session-newer': 200, 'session-opened': 950 }
  const filter = ctx.sessionQuery.filterSessions.bind(ctx.sessionQuery)
  vi.spyOn(ctx.sessionQuery, 'filterSessions').mockImplementation(async (filters, signal) => {
    const records = await filter(filters, signal)
    return records.map(record => ({ ...record, header: { ...record.header,
      createdAt: created[record.header.id] ?? record.header.createdAt } })).reverse()
  })
  const read = ctx.sessionQuery.readTitleSnapshots.bind(ctx.sessionQuery)
  // Pinned for every saved session: the real reads report each log's newest event, which is now.
  const used: Record<string, number> = { 'session-older': 900, 'session-blank': 500, 'session-newer': 300, 'session-opened': 950 }
  vi.spyOn(ctx.sessionQuery, 'readTitleSnapshots').mockImplementation(async (ids, signal) =>
    (await read(ids, signal)).map(result => result.status !== 'fulfilled' || used[result.sessionId] === undefined ? result
      : { ...result, value: { ...result.value, lastEventAt: used[result.sessionId]! } }))
  const prompt = await picker(navigation)
  expect(prompt.choices.map(choice => choice.value)).toEqual([active, 'session-older', 'session-blank', 'session-newer', ''])
  expect(prompt.choices.map(choice => choice.role)).toEqual([
    'session-current', 'session-saved', 'session-saved', 'session-saved', 'session-new',
  ])
  // Enter goes back to the session used last, not the one already open.
  expect(prompt.initial).toBe('session-older')
  // An untitled session is named as one, with a short id beside its age, rather than by its id.
  const blank = prompt.choices.find(choice => choice.value === 'session-blank')!
  expect(blank.label).toBe(copy.untitledSession)
  expect(blank.description).toMatch(/ · session-blank$/)
  expect(prompt.choices.find(choice => choice.value === 'session-older')!.label).toBe('older')
})

it('points at the open session when there is no other', async () => {
  const { navigation } = await connected()
  const prompt = await picker(navigation)
  expect(prompt.initial).toBe(navigation.controller!.agent.id)
  expect(prompt.choices[0]).toMatchObject({ role: 'session-current', label: copy.untitledSession })
  navigation.cancel()
  await vi.waitFor(() => expect(navigation.busy).toBe(false))
})

it('filters other workspaces, subagents, and live owners while retaining sessions with unreadable titles', async () => {
  const { ctx, navigation } = await connected()
  const other = await openSession(ctx, {}, new AbortController().signal, () => {})
  const detached = await openSession(ctx, { preset: 'other' }, new AbortController().signal, () => {})
  detached.agent.followup(user('Saved session'))
  await detached.agent.whenIdle()
  await detached.dispose()
  const hidden: SessionId[] = []
  for (const [suffix, meta] of [
    ['foreign', { cwd: '/another-workspace' }],
    ['child', { cwd: process.cwd(), origin: 'subagent' as const }],
  ] as const) {
    const handle = await ctx.agents.create({
      sessionId: brandString<SessionId>(`${detached.agent.id}-${suffix}`), meta,
      agentOptions: { provider: 'mock', model: 'model' },
    })
    handle.agent.session.append('session/title', { title: suffix, messageSeqs: [], source: { kind: 'user' } })
    await handle.dispose()
    hidden.push(handle.agent.id)
  }
  vi.spyOn(ctx.sessionQuery, 'filterSessions')
  vi.spyOn(ctx.sessionQuery, 'readTitleSnapshots').mockResolvedValue([{ status: 'rejected', sessionId: detached.agent.id, reason: new Error('unreadable') }])
  const prompt = await picker(navigation)
  expect(ctx.sessionQuery.filterSessions).toHaveBeenCalledWith([{ kind: 'cwd', values: [process.cwd()] }], expect.any(AbortSignal))
  expect(prompt.choices.filter(choice => choice.value === detached.agent.id)).toHaveLength(1)
  expect(prompt.choices.some(choice => choice.value === other.agent.id)).toBe(false)
  expect(prompt.choices.some(choice => hidden.some(id => id === choice.value))).toBe(false)
  expect(prompt.warning).toBe(copy.sessionTitlesUnavailable)
  navigation.cancel()
  await vi.waitFor(() => expect(navigation.busy).toBe(false))
})

it.each(['cancel', 'close'] as const)('suppresses a late session listing after %s and drains it', async mode => {
  const { ctx, navigation, changed, model } = await connected()
  const entered = Promise.withResolvers<AbortSignal | undefined>()
  const release = Promise.withResolvers<void>()
  cleanup.push(async () => { release.resolve(); await navigation.controller?.drain() })
  const original = ctx.sessionQuery.filterSessions.bind(ctx.sessionQuery)
  vi.spyOn(ctx.sessionQuery, 'filterSessions').mockImplementation(async (filters, signal) => {
    entered.resolve(signal)
    await release.promise
    return original(filters)
  })
  navigation.submit('/sessions')
  const signal = await entered.promise
  const first = navigation.controller!
  if (mode === 'close') navigation.close()
  else navigation.cancel()
  expect(signal?.aborted).toBe(true)
  const paints = changed.mock.calls.length
  release.resolve()
  await vi.waitFor(() => expect(navigation.busy).toBe(false))
  expect(navigation.controller).toBe(first)
  expect(first.view.interaction).toBeUndefined()
  if (mode === 'close') expect(changed).toHaveBeenCalledTimes(paints)
  else expect(first.view.notice).toBe(copy.sessionsCancelled)
  expect(model.requests).toEqual([])
})

it.each(['running', 'pending'] as const)('refuses navigation while the old agent has %s work', async state => {
  const { ctx, navigation, model } = await connected()
  const first = navigation.controller!
  const entered = Promise.withResolvers<void>()
  model.response = async function* (options) {
    entered.resolve()
    await new Promise<void>(resolve => options.signal!.addEventListener('abort', () => resolve(), { once: true }))
    options.signal!.throwIfAborted()
  }
  if (state === 'running') { navigation.submit('Start'); await entered.promise }
  else first.agent.inject(createUserMessage({ content: [{ type: 'text', text: 'Plugin context' }], source: { kind: 'plugin', plugin: 'test' } }))
  const list = vi.spyOn(ctx.sessionQuery, 'filterSessions')
  navigation.submit('/sessions')
  await first.drain()
  await vi.waitFor(() => expect(navigation.busy).toBe(false))
  // The command itself fails, so its transcript record says why.
  expect(transcriptRows(first.view.committed).at(-1)).toEqual({ kind: 'notice', placement: 'command', tone: 'error',
    text: state === 'running' ? copy.sessionsIdle : copy.sessionsPending })
  expect(list).not.toHaveBeenCalled()
  expect(navigation.controller).toBe(first)
  if (state === 'running') { first.agent.cancel({ kind: 'user' }, { keepInbox: true }); await first.agent.whenIdle() }
  else expect(first.agent.inbox.nextStep).toHaveLength(1)
})

it('cancels a displayed picker when inbox state changes and does not consume the new input', async () => {
  const { navigation } = await connected()
  const prompt = await picker(navigation)
  const first = navigation.controller!
  first.agent.inbox.append('next-turn', user('Retain this input'))
  first.interactions.answer(prompt.id, '')
  await vi.waitFor(() => expect(navigation.busy).toBe(false))
  expect(navigation.controller).toBe(first)
  expect(first.view.interaction).toBeUndefined()
  expect(first.view.pending).toEqual([expect.objectContaining({ text: 'Retain this input' })])
  expect(first.view.notice).toContain(copy.sessionsPending)
})

it.each(['cancel', 'failure'] as const)('rolls back a prepared replacement on replay %s', async mode => {
  const { ctx, navigation, model } = await connected()
  const first = navigation.controller!
  const entered = Promise.withResolvers<void>()
  const release = Promise.withResolvers<void>()
  cleanup.push(async () => { release.resolve() })
  const original = ctx.sessionQuery.observeSession.bind(ctx.sessionQuery)
  vi.spyOn(ctx.sessionQuery, 'observeSession').mockImplementation(async (id, options) => {
    if (id === first.agent.id) return original(id, options)
    entered.resolve()
    await release.promise
    if (mode === 'failure') throw new Error('Replay unavailable')
    return original(id, options)
  })
  const prompt = await picker(navigation)
  first.interactions.answer(prompt.id, '')
  await entered.promise
  navigation.submit('Must not reach either session')
  if (mode === 'cancel') navigation.cancel()
  release.resolve()
  await vi.waitFor(() => expect(navigation.busy).toBe(false))
  expect(navigation.controller).toBe(first)
  expect(model.requests).toEqual([])
  expect((await ctx.sessionQuery.listSessions()).filter(record => record.live).map(record => record.header.id)).toEqual([first.agent.id])
  expect(first.view.notice).toContain(mode === 'cancel' ? copy.sessionsCancelled : 'Replay unavailable')
  vi.restoreAllMocks()
  await select(navigation, '')
  expect(navigation.controller).not.toBe(first)
})

it('retains the old owner when a selected session gains another live owner before resume', async () => {
  const { ctx, navigation } = await connected()
  const first = navigation.controller!
  const saved = await openSession(ctx, {}, new AbortController().signal, () => {})
  saved.agent.followup(user('Saved'))
  await saved.agent.whenIdle()
  await saved.dispose()
  const prompt = await picker(navigation)
  const owner = await openSession(ctx, { resume: saved.agent.id }, new AbortController().signal, () => {})
  first.interactions.answer(prompt.id, saved.agent.id)
  await vi.waitFor(() => expect(navigation.busy).toBe(false))
  expect(navigation.controller).toBe(first)
  expect(ctx.agents.get(owner.agent.id)).toBe(owner.agent)
  expect(first.view.notice).toContain('live owner')
})

it('drains a canceled preset mount and prevents a late controller from attaching', async () => {
  const { ctx, navigation, changed } = await connected()
  const first = navigation.controller!
  const entered = Promise.withResolvers<void>()
  const release = Promise.withResolvers<void>()
  cleanup.push(async () => { release.resolve() })
  const mount = ctx.agentPresets.mount.bind(ctx.agentPresets)
  vi.spyOn(ctx.agentPresets, 'mount').mockImplementation(async (...args) => {
    entered.resolve()
    await release.promise
    return mount(...args)
  })
  const prompt = await picker(navigation)
  first.interactions.answer(prompt.id, '')
  await entered.promise
  navigation.close()
  const paints = changed.mock.calls.length
  const drained = vi.fn()
  const done = navigation.drain().then(drained)
  await Promise.resolve()
  expect(drained).not.toHaveBeenCalled()
  release.resolve()
  await done
  expect(changed).toHaveBeenCalledTimes(paints)
  expect((await ctx.sessionQuery.listSessions()).filter(record => record.live)).toEqual([])
})
