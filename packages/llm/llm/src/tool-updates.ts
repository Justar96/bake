/** Provider-specific projection of durable tool changes without altering conversation messages. */

import type { Message } from './message.ts'
import type { ToolDeclaration, ToolHistory, ToolSchema, ToolUpdate, ToolUpdateNotice } from './types.ts'

/** Declarations and native changes for one provider request. */
export interface ProjectedToolUpdates {
  readonly tools: ToolDeclaration[] | undefined
  readonly toolUpdates: readonly ToolUpdateNotice[] | undefined
}

/**
 * Preserve historical declarations when this route and the selected message prefix support every update.
 * Missing anchors, mismatched active tools, or unsupported routes use full current declarations.
 * @param messages - selected conversation history, including auxiliary-call prefixes.
 * @param tools - current active definitions.
 * @param mode - exact route capability.
 * @param history - immutable Session fold.
 * @returns declarations plus native changes, or complete declarations without changes.
 */
export function projectToolUpdates(
  messages: readonly Message[],
  tools: readonly ToolSchema[] | undefined,
  mode: ToolUpdate | undefined,
  history?: ToolHistory,
): ProjectedToolUpdates {
  const fallback = (): ProjectedToolUpdates => ({
    tools: tools?.some(tool => (tool as ToolDeclaration).deferLoading === true) ? tools.map((tool) => {
      const { deferLoading: _loading, ...schema } = tool as ToolDeclaration
      return schema
    }) : tools as ToolDeclaration[] | undefined,
    toolUpdates: undefined,
  })
  if (mode === undefined || history === undefined) return fallback()
  const positions = new Map(messages.map((message, index) => [message.id, index]))
  const active = new Map(history.tools.map(tool => [tool.name, tool]))
  const declarations = new Map<string, ToolDeclaration>(active)
  let previousPosition = -1
  for (const update of history.updates) {
    const position = positions.get(update.afterMessageId)
    if (position === undefined || position < previousPosition || messages[position]?.role !== 'user') return fallback()
    previousPosition = position
    for (const tool of update.additions) {
      const declared = declarations.get(tool.name)
      if (declared !== undefined) {
        const { deferLoading: _loading, ...schema } = declared
        if (JSON.stringify(schema) !== JSON.stringify(tool)) return fallback()
      } else {
        declarations.set(tool.name, { ...tool, deferLoading: true })
      }
      active.set(tool.name, tool)
    }
    for (const name of update.removals) active.delete(name)
  }
  if (active.size !== (tools?.length ?? 0)
    || tools?.some(tool => JSON.stringify(tool) !== JSON.stringify(active.get(tool.name)))) return fallback()
  if (mode === 'addition-only') {
    for (const name of declarations.keys()) if (!active.has(name)) declarations.delete(name)
  }
  const offered = new Set(history.tools.map(tool => tool.name))
  const toolUpdates = history.updates.flatMap((update): ToolUpdateNotice[] => {
    const additions = update.additions.flatMap((tool) => {
      if (!declarations.has(tool.name) || offered.has(tool.name)) return []
      offered.add(tool.name)
      return [tool.name]
    })
    const removals = update.removals.filter(name => mode === 'in-history' && offered.delete(name))
    return additions.length + removals.length === 0 ? [] : [{ afterMessageId: update.afterMessageId, additions, removals }]
  })
  return {
    tools: declarations.size === 0 && tools === undefined ? undefined : [...declarations.values()],
    toolUpdates: toolUpdates.length === 0 ? undefined : toolUpdates,
  }
}
