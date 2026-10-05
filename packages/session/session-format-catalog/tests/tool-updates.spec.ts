/** Vocabulary growth preserves V3 grammar and requires older readers to refuse the new event. */
import { expect, it } from 'vitest'
import { createUserMessage } from '@deepseek-ai/dsh-llm'
import { Session, SessionId, SessionLogOffset, KNOWN_SESSION_EVENT_TYPES } from 'bake-session'
import type { SessionEvent } from 'bake-session'
import { restoreReleasedV3Artifact } from '@deepseek-ai/dsh-session-format-v2-to-v3'
import { sessionFormatCatalog } from '../src/index.ts'

it('round-trips required dynamic tool updates without changing the released message grammar', () => {
  const session = Session.create(SessionId('tool-update-format'))
  const config = { provider: 'test', model: 'test' }
  session.append('turn/start', { turn: 1 })
  session.append('request/header', { header: { config }, reason: 'initial' })
  const input = session.append('user/message', createUserMessage({ source: { kind: 'user' }, content: [{ type: 'text', text: 'continue' }] }), { surfaceOp: 'append' })
  const header = session.append('request/header', {
    header: { config, tools: [{ name: 'search', description: '', parameters: {} }] }, reason: 'change',
  })
  session.append('request/tool-update', { headerSeq: header.seq, afterMessageId: input.data.id, additions: ['search'], removals: [] })
  session.append('turn/end', { turn: 1, reason: { kind: 'completed' } })
  const original = JSON.stringify(session.snapshotEvents())
  const physicalHeader = sessionFormatCatalog.encodeCurrentHeader({ ...session.header, delegationDepth: 0 }, 0)
  expect(physicalHeader['version']).toBe(3)
  const restore = sessionFormatCatalog.createRestore(physicalHeader, { recovery: 'strict', validation: 'current' })
  for (const event of session.snapshotEvents()) restore.decodeRow(sessionFormatCatalog.encodeCurrentEvent(event))
  const artifact = restore.finish()
  const reopened = Session.fromRestore(session.id, artifact.events as SessionEvent[], session.header, SessionLogOffset(0), 'detached')
  expect(reopened.toolHistory()).toEqual(session.toolHistory())
  expect(reopened.deriveMessages()).toEqual(session.deriveMessages())
  expect(JSON.stringify(session.snapshotEvents())).toBe(original)
  const olderVocabulary = new Set([...KNOWN_SESSION_EVENT_TYPES].filter(type => type !== 'request/tool-update'))
  expect(() => restoreReleasedV3Artifact(artifact, olderVocabulary)).toThrow(/unknown event type.*request\/tool-update/)
})
