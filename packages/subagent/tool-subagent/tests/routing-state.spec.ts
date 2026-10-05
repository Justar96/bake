import { describe, expect, it } from 'vitest'
import { Context } from '@deepseek-ai/cordis'
import { ToolCallId } from '@deepseek-ai/dsh-llm'
import { Session, SessionId, SessionLogOffset } from 'bake-session'
import SessionProjectionRegistry from '@deepseek-ai/dsh-session-projection'
import { recordSubagentRoutingDecision, subagentRoutingDecision, subagentRoutingProjectionDefinition } from '../src/routing-state.ts'
import type { SubagentRoutingDecision } from '../src/types.ts'

const decision: SubagentRoutingDecision = {
  childId: SessionId('child'), callId: ToolCallId('call-1'), source: 'auto',
  route: { provider: 'alpha', model: 'fast-model', reasoningEffort: 'low' },
  router: { reason: 'bounded task', fallback: false,
    assessment: { policy: '2026-10-01', status: 'normal', difficulty: 0.2, reasons: [] } },
}

describe('durable subagent routing decisions', () => {
  it('restores display evidence without adding model messages', async () => {
    const ctx = new Context()
    await ctx.plugin(SessionProjectionRegistry)
    ctx.sessionProjections.register(subagentRoutingProjectionDefinition)
    try {
      const parent = Session.create(SessionId('parent'))
      recordSubagentRoutingDecision(parent, decision)
      const restored = Session.fromRestore(parent.id, JSON.parse(JSON.stringify(parent.snapshotEvents())),
        parent.header, SessionLogOffset(0), 'detached')
      expect(subagentRoutingDecision(ctx.sessionProjections, restored, decision.childId)).toEqual(decision)
      expect(ctx.sessionProjections.snapshot(restored).values['subagentRoutingDecisions']).toEqual({ child: decision })
      expect(restored.snapshotEvents()[0]).toMatchObject({ type: 'subagent/routing-decision', ignorable: true })
      expect(parent.deriveMessages()).toEqual([])
      expect(restored.deriveMessages()).toEqual([])
    } finally {
      await ctx.fiber.dispose()
    }
  })

  it('excludes inherited decisions from live and restored fork children', async () => {
    const ctx = new Context()
    await ctx.plugin(SessionProjectionRegistry)
    ctx.sessionProjections.register(subagentRoutingProjectionDefinition)
    try {
      const parent = Session.create(SessionId('parent'))
      recordSubagentRoutingDecision(parent, decision)
      const inherited = parent.snapshotEvents()
      const forkId = SessionId('fork')
      const fork = Session.create(forkId, inherited, { ...parent.header, id: forkId, parentSession: parent.id, isSeeded: true },
        SessionLogOffset(inherited.length))
      expect(subagentRoutingDecision(ctx.sessionProjections, fork, decision.childId)).toBeUndefined()
      const own = { ...decision, childId: SessionId('grandchild'), callId: ToolCallId('call-2') }
      recordSubagentRoutingDecision(fork, own)
      const restored = Session.fromRestore(fork.id, JSON.parse(JSON.stringify(fork.snapshotEvents())),
        fork.header, fork.inheritedEventCount, 'detached')
      expect(subagentRoutingDecision(ctx.sessionProjections, restored, decision.childId)).toBeUndefined()
      expect(subagentRoutingDecision(ctx.sessionProjections, restored, own.childId)).toEqual(own)
      expect(ctx.sessionProjections.snapshot(restored).values['subagentRoutingDecisions']).toEqual({ grandchild: own })
    } finally {
      await ctx.fiber.dispose()
    }
  })

  it('removes projection registration on owning fiber disposal and rebuilds from the log', async () => {
    const ctx = new Context()
    await ctx.plugin(SessionProjectionRegistry)
    const plugin = Object.assign((owner: Context) => {
      owner.sessionProjections.register(subagentRoutingProjectionDefinition)
    }, { inject: ['sessionProjections'] })
    const fiber = await ctx.plugin(plugin)
    const parent = Session.create(SessionId('parent'))
    recordSubagentRoutingDecision(parent, decision)
    expect(ctx.sessionProjections.snapshot(parent).values['subagentRoutingDecisions']).toEqual({ child: decision })
    await fiber.dispose()
    expect(ctx.sessionProjections.snapshot(parent).values).not.toHaveProperty('subagentRoutingDecisions')
    await ctx.plugin(plugin)
    expect(subagentRoutingDecision(ctx.sessionProjections, parent, decision.childId)).toEqual(decision)
    await ctx.fiber.dispose()
  })
})
