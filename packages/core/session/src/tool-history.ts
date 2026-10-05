/** Validation and reconstruction of logged native tool updates. */

import type { ToolHistory, ToolSchema } from '@deepseek-ai/dsh-llm'
import { deepFreeze } from 'bake-util-values'
import type { SessionEvent, SessionSeq } from './types.ts'
import type { SurfaceManager } from './surface.ts'

/** Validate the event-local payload before persistence or independent event adoption. */
export function validateToolUpdateData(data: unknown, subject: string): void {
  const value = data as SessionEvent<'request/tool-update'>['data'] | null
  const names = (input: unknown): input is string[] => Array.isArray(input)
    && input.every(name => typeof name === 'string' && name.length > 0)
    && new Set(input).size === input.length
  if (typeof value !== 'object' || value === null || Array.isArray(value)
    || !Number.isSafeInteger(value.headerSeq) || value.headerSeq < 0 || Object.is(value.headerSeq, -0)
    || typeof value.afterMessageId !== 'string' || value.afterMessageId.length === 0
    || !names(value.additions) || !names(value.removals)
    || value.additions.length + value.removals.length === 0
    || value.additions.some(name => value.removals.includes(name))) {
    throw new Error(`${subject} has an invalid tool update`)
  }
}

/** Validate historical references against committed state before admitting an update. */
export function validateToolUpdate(
  event: SessionEvent,
  events: readonly SessionEvent[],
  surface: SurfaceManager,
): void {
  if (event.type !== 'request/tool-update') return
  if (event.ignorable === true) throw new Error('request/tool-update must be required on read')
  const { headerSeq, afterMessageId, additions, removals } = event.data
  const header = events[headerSeq]
  if (headerSeq >= event.seq || header?.type !== 'request/header') {
    throw new Error('request/tool-update must reference an earlier request header')
  }
  for (let i = headerSeq + 1; i < event.seq; i++) {
    const next = events[i]
    if (next?.type === 'request/header' || next?.type === 'request/tool-update') {
      throw new Error('request/tool-update must reference the latest, unused request header')
    }
  }
  let previous: readonly ToolSchema[] | undefined
  for (let i = headerSeq - 1; i >= 0; i--) {
    const before = events[i]
    if (before?.type === 'request/header') {
      previous = before.data.header.tools ?? []
      break
    }
  }
  if (previous === undefined) throw new Error('request/tool-update requires a preceding tool baseline')
  const current = header.data.header.tools ?? []
  const before = new Set(previous.map(tool => tool.name))
  const after = new Set(current.map(tool => tool.name))
  const expectedAdditions = current.filter(tool => !before.has(tool.name)).map(tool => tool.name)
  const expectedRemovals = previous.filter(tool => !after.has(tool.name)).map(tool => tool.name)
  if (JSON.stringify(additions) !== JSON.stringify(expectedAdditions)
    || JSON.stringify(removals) !== JSON.stringify(expectedRemovals)) {
    throw new Error('request/tool-update must match the referenced header change')
  }
  for (let i = surface.nodes.length - 1; i >= 0; i--) {
    const node = surface.nodes[i]
    const source = node === undefined ? undefined : events[node]
    if (source === undefined) continue
    const message = surface.deriveEventMessage(source)
    if (message === null) continue
    if (message.role === 'system') continue
    if (message.id !== afterMessageId || message.role !== 'user') break
    return
  }
  throw new Error('request/tool-update must follow the current user or tool-result message')
}

/** Folds committed headers and updates independently of provider capabilities. */
export class ToolHistoryProjection {
  private headerSeq: SessionSeq | undefined
  private baselineSeq: SessionSeq | undefined
  private active: readonly ToolSchema[] = []
  private declared = new Map<string, ToolSchema>()
  private available = new Set<string>()
  private history: ToolHistory = deepFreeze({ tools: [], updates: [] })

  /** Consume one validated event; previously returned snapshots remain immutable. */
  apply(event: SessionEvent): void {
    if (event.type === 'request/header') {
      const tools = event.data.header.tools ?? []
      const redeclared = tools.some((tool) => {
        const before = this.declared.get(tool.name)
        return before !== undefined && JSON.stringify(before) !== JSON.stringify(tool)
      })
      const incomplete = this.active.length !== this.available.size
        || this.active.some(tool => !this.available.has(tool.name))
      if (this.baselineSeq === undefined || event.data.reason === 'series' || event.data.startsSeries || redeclared || incomplete) {
        this.baselineSeq = event.seq
        this.declared = new Map(tools.map(tool => [tool.name, tool]))
        this.available = new Set(tools.map(tool => tool.name))
        this.history = deepFreeze({ tools, updates: [] })
      }
      this.active = tools
      this.headerSeq = event.seq
    } else if (event.type === 'request/tool-update') {
      if (event.data.headerSeq === this.baselineSeq) return
      if (event.data.headerSeq !== this.headerSeq) throw new Error('tool history: update references a stale header')
      const additions = event.data.additions.map((name) => {
        const tool = this.active.find(tool => tool.name === name)
        if (tool === undefined) throw new Error(`tool history: missing definition for ${name}`)
        this.declared.set(name, tool)
        this.available.add(name)
        return tool
      })
      for (const name of event.data.removals) this.available.delete(name)
      this.history = deepFreeze({
        tools: this.history.tools,
        updates: [...this.history.updates, { afterMessageId: event.data.afterMessageId, additions, removals: event.data.removals }],
      })
    }
  }

  /** Missing updates in historical logs or a crash tail select complete active declarations. */
  snapshot(): ToolHistory {
    if (this.active.length !== this.available.size || this.active.some(tool => !this.available.has(tool.name))) {
      return deepFreeze({ tools: this.active, updates: [] })
    }
    return this.history
  }
}
