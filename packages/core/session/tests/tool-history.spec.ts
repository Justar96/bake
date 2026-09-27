import { describe, expect, it } from 'vitest'
import { createUserMessage, MessageId, projectToolUpdates } from '@deepseek-ai/dsh-llm'
import type { ToolSchema } from '@deepseek-ai/dsh-llm'
import { Session, SessionId, SessionSeq, canonicalHeader } from '../src/index.ts'
import type { RequestHeaderReason, SessionEvent } from '../src/index.ts'

const tool = (name: string, description = name): ToolSchema => ({ name, description, parameters: { type: 'object' } })
const header = (session: Session, tools: ToolSchema[], reason: RequestHeaderReason = 'change', startsSeries?: true) =>
  session.append('request/header', {
    header: canonicalHeader({ config: { provider: 'test', model: 'test' }, tools }), reason,
    ...startsSeries ? { startsSeries } : {},
  })
function input(session: Session) {
  return session.append('user/message', createUserMessage({ source: { kind: 'user' }, content: [{ type: 'text', text: 'continue' }] }), { surfaceOp: 'append' })
}
function change(session: Session, tools: ToolSchema[], additions: string[], removals: string[]) {
  const anchor = input(session)
  const next = header(session, tools)
  return session.append('request/tool-update', { headerSeq: next.seq, afterMessageId: anchor.data.id, additions, removals })
}

describe('tool history', () => {
  it('retains immutable definitions through removal, re-addition, and restoration', () => {
    const session = Session.create(SessionId('history'))
    expect(session.toolHistory()).toEqual({ tools: [], updates: [] })
    header(session, [tool('search')], 'initial')
    const first = session.toolHistory()
    change(session, [tool('search'), tool('fetch')], ['fetch'], [])
    const second = session.toolHistory()
    change(session, [tool('search')], [], ['fetch'])
    change(session, [tool('search'), tool('fetch')], ['fetch'], [])
    const history = session.toolHistory()
    expect(first).toEqual({ tools: [tool('search')], updates: [] })
    expect(second.updates).toHaveLength(1)
    expect(history.updates.map(update => [update.additions.map(value => value.name), update.removals])).toEqual([
      [['fetch'], []], [[], ['fetch']], [['fetch'], []],
    ])
    expect(Object.isFrozen(history.updates[0]?.additions[0]?.parameters)).toBe(true)
    expect(() => { (history.updates as unknown[]).push({}) }).toThrow()
    const restored = Session.create(SessionId('restored'), session.snapshotEvents())
    header(restored, [tool('search'), tool('fetch')], 'resume')
    expect(restored.toolHistory()).toEqual(history)
    expect(restored.deriveMessages()).toEqual(session.deriveMessages())
  })

  it('resets changed definitions, explicit series and resumed compaction while preserving old snapshots', () => {
    const session = Session.create(SessionId('reset'))
    header(session, [tool('search')], 'initial')
    change(session, [tool('search'), tool('fetch')], ['fetch'], [])
    change(session, [tool('search')], [], ['fetch'])
    const previous = session.toolHistory()
    change(session, [tool('search'), tool('fetch', 'changed schema')], ['fetch'], [])
    expect(session.toolHistory()).toEqual({ tools: [tool('search'), tool('fetch', 'changed schema')], updates: [] })
    expect(previous.updates).toHaveLength(2)
    change(session, [tool('fetch', 'changed schema')], [], ['search'])
    header(session, [tool('fetch', 'changed schema')], 'series')
    expect(session.toolHistory().updates).toEqual([])
    change(session, [tool('search'), tool('fetch', 'changed schema')], ['search'], [])
    header(session, [tool('search'), tool('fetch', 'changed schema')], 'resume', true)
    expect(session.toolHistory().updates).toEqual([])
  })

  it('falls back for old logs and crash tails with unrecorded changes', () => {
    const session = Session.create(SessionId('old'))
    header(session, [tool('search')], 'initial')
    header(session, [tool('fetch')])
    expect(session.toolHistory()).toEqual({ tools: [tool('fetch')], updates: [] })
    const restored = Session.create(SessionId('old-restored'), session.snapshotEvents())
    header(restored, [tool('fetch')], 'resume')
    change(restored, [tool('fetch'), tool('new')], ['new'], [])
    expect(restored.toolHistory().tools).toEqual([tool('fetch')])
    expect(restored.toolHistory().updates[0]?.additions).toEqual([tool('new')])
  })

  it('falls back when compaction removes a recorded anchor before the next header', () => {
    const session = Session.create(SessionId('compact'))
    header(session, [tool('search')], 'initial')
    change(session, [tool('search'), tool('fetch')], ['fetch'], [])
    const seq = session.surface.nodes.at(-1)!
    session.append('user/message', createUserMessage({ source: { kind: 'plugin', plugin: 'compaction' }, content: [{ type: 'text', text: 'summary' }] }), {
      surfaceOp: { op: 'replace', startSeq: seq, endSeq: seq }, sourceEventSeqs: [seq],
    })
    expect(projectToolUpdates(session.deriveMessages(), session.requestHeader()?.tools, 'in-history', session.toolHistory())).toEqual({
      tools: [tool('search'), tool('fetch')], toolUpdates: undefined,
    })
  })

  it('rejects malformed or stale references before append or restoration changes state', () => {
    const session = Session.create(SessionId('validation'))
    header(session, [tool('search')], 'initial')
    const anchor = input(session)
    const next = header(session, [tool('fetch')])
    const valid = { headerSeq: next.seq, afterMessageId: anchor.data.id, additions: ['fetch'], removals: ['search'] }
    for (const invalid of [
      { ...valid, headerSeq: SessionSeq(99) },
      { ...valid, headerSeq: anchor.seq },
      { ...valid, afterMessageId: MessageId('missing') },
      { ...valid, additions: [] },
      { ...valid, additions: ['missing'] },
      { ...valid, additions: ['fetch', 'fetch'] },
      { ...valid, removals: ['fetch'] },
      { ...valid, additions: [], removals: [] },
    ]) {
      expect(() => session.append('request/tool-update', invalid)).toThrow(/tool.update/)
      expect(session.seq).toBe(3)
      const event = { type: 'request/tool-update', seq: SessionSeq(3), time: 0, data: invalid } as SessionEvent
      expect(() => Session.create(SessionId('invalid-seed'), [...session.snapshotEvents(), event])).toThrow(/tool.update/)
    }
    const event = session.append('request/tool-update', valid)
    expect(() => session.append('request/tool-update', valid)).toThrow(/latest, unused/)
    const seed = session.snapshotEvents().map(item => item.seq === event.seq ? { ...item, ignorable: true as const } : item)
    expect(() => Session.create(SessionId('ignorable'), seed)).toThrow(/required on read/)
  })
})
