/** Model discovery and selection validation delegated to the Harness LLM catalog. */
import type { ModelSelection } from '@deepseek-ai/dsh-agent'
import type { LlmRuntime, LlmResolvedModelInfo } from '@deepseek-ai/dsh-llm'

/**
 * Format a model selection for the composer and status line.
 * @param selection - selected provider and model.
 * @returns the provider/model route.
 */
export const routeOf = (selection: ModelSelection): string => `${selection.provider}/${selection.model}`

/** One advertised route; catalog membership is advisory. */
export interface CatalogLine {
  readonly route: string
  readonly name: string
  readonly current: boolean
}

/** Available routes and providers whose catalogs could not be read. */
export interface ModelCatalog {
  readonly entries: readonly CatalogLine[]
  readonly unavailable: readonly string[]
}

/**
 * Discover advertised models, retaining a current route absent from advisory catalogs.
 * @param llm - Harness LLM service.
 * @param current - the session's live selection.
 * @param signal - stops publication and further reads; an active listModels call must settle before return.
 * @returns routes in provider order and explicit partial-discovery diagnostics.
 */
export async function listRoutes(llm: LlmRuntime, current: ModelSelection, signal: AbortSignal): Promise<ModelCatalog> {
  const entries: CatalogLine[] = []
  const unavailable: string[] = []
  for (const provider of llm.listProviders()) {
    signal.throwIfAborted()
    try {
      const models = await llm.listModels(provider.id)
      signal.throwIfAborted()
      for (const model of models) {
        const route = `${provider.id}/${model.id}`
        entries.push({ route, name: model.name, current: route === routeOf(current) })
      }
    } catch (_providerCatalogUnavailable) {
      signal.throwIfAborted()
      unavailable.push(provider.id)
    }
  }
  if (!entries.some(entry => entry.current)) entries.unshift({ route: routeOf(current), name: current.model, current: true })
  return { entries, unavailable }
}

/**
 * Read capabilities for one exact route; provider/model ids can contain further slashes.
 * @param llm - Harness LLM service.
 * @param route - provider/model text.
 * @param signal - cancellation for the exact-model lookup.
 * @returns resolved metadata, or undefined when the route cannot be resolved.
 */
export async function resolveRoute(llm: LlmRuntime, route: string, signal: AbortSignal): Promise<LlmResolvedModelInfo | undefined> {
  signal.throwIfAborted()
  const separator = route.indexOf('/')
  if (separator <= 0 || separator === route.length - 1) return undefined
  try {
    const info = await llm.resolveModelInfo(route.slice(0, separator), route.slice(separator + 1), signal)
    signal.throwIfAborted()
    return info
  } catch (_unresolvedModel) {
    signal.throwIfAborted()
    return undefined
  }
}

/** A validated selection or the reason the requested route/effort was refused. */
export type SelectionResult =
  | { readonly kind: 'selected'; readonly selection: ModelSelection }
  | { readonly kind: 'unknown-route'; readonly route: string }
  | { readonly kind: 'unknown-effort'; readonly offered: readonly string[] }

/**
 * Validate the requested model and effort without changing live selection.
 * @param llm - Harness LLM service.
 * @param route - exact provider/model text.
 * @param effort - provider-owned effort id, or undefined for provider defaults.
 * @param signal - cancellation for capability discovery.
 * @returns a selection the caller may install after checking its current lifecycle state.
 */
export async function resolveSelection(llm: LlmRuntime, route: string, effort: string | undefined, signal: AbortSignal): Promise<SelectionResult> {
  const info = await resolveRoute(llm, route, signal)
  if (info === undefined) return { kind: 'unknown-route', route }
  const offered = info.reasoning?.efforts ?? []
  const chosen = offered.find(item => item.id === effort)
  if (effort !== undefined && chosen === undefined) return { kind: 'unknown-effort', offered: offered.map(item => item.id) }
  return { kind: 'selected', selection: { provider: info.provider, model: info.id,
    ...chosen === undefined ? {} : { reasoningEffort: chosen.id },
  } }
}
