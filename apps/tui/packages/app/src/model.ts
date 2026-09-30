/** Model discovery and selection validation delegated to the Harness LLM catalog. */
import type { ModelSelection } from '@deepseek-ai/dsh-agent'
import type { LlmModelInfo, LlmRuntime, LlmModelReasoningInfo, LlmResolvedModelInfo } from '@deepseek-ai/dsh-llm'
import type { TuiCopy } from '@dsh-tui/ui/copy.ts'
import { formatTokens } from '@dsh-tui/ui/format.ts'
import type { Choice, ChoicePrompt } from '@dsh-tui/ui/picker.tsx'

/**
 * Format a model selection for the composer and status line.
 * @param selection - selected provider and model.
 * @returns the provider/model route.
 */
export const routeOf = (selection: ModelSelection): string => `${selection.provider}/${selection.model}`

/**
 * Whether a catalog's display name only restates its route, as a catalog that
 * names models by id does. `deepseek-v4-flash` restates
 * `deepseek/deepseek-v4-flash`. The picker drops such a name instead of
 * printing the model twice on a row.
 * @param name - the catalog's display name.
 * @param route - the provider/model route.
 * @returns true when the name, ignoring case and separators, is the route or its model.
 */
export function namesRoute(name: string, route: string): boolean {
  const plain = (text: string): string => text.toLowerCase().replace(/[\s._/-]/g, '')
  const model = route.slice(route.indexOf('/') + 1)
  return plain(name) === '' || plain(name) === plain(route) || plain(name) === plain(model)
}

/** One advertised route. Catalog membership is advisory. */
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
 * @param signal - stops publication and further reads. An active `listModels` call must settle before return.
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

/** How many models the sheet's Recent group keeps. */
export const RECENT_MODELS = 5

/** Models the user chose before, which the `/model` sheet leads with. */
export interface RecentModels {
  /** Routes, most recent first. */
  readonly recentModels: readonly string[]
  /**
   * Put a chosen route first, keeping at most {@link RECENT_MODELS}.
   * @param route - the provider/model route just chosen.
   */
  rememberModel(route: string): Promise<void>
}

/**
 * The routes a Recent list holds once `route` is chosen.
 * @param recent - routes, most recent first.
 * @param route - the route just chosen.
 * @returns `route` first, without repeats, at most {@link RECENT_MODELS}.
 */
export function withRecent(recent: readonly string[], route: string): readonly string[] {
  return [route, ...recent.filter(entry => entry !== route)].slice(0, RECENT_MODELS)
}

/** One model on the `/model` sheet, with the facts its row shows. */
export interface SheetModel {
  readonly route: string
  readonly provider: string
  readonly model: string
  readonly name: string
  readonly current: boolean
  /** Combined context window in tokens, when the adapter knows it. */
  readonly context?: number
  /** Whether it accepts images; absent when the catalog does not say. */
  readonly image?: boolean
  readonly reasoning?: LlmModelReasoningInfo
}

/** One provider's models, newest first. */
export interface SheetGroup {
  readonly provider: string
  readonly name: string
  readonly models: readonly SheetModel[]
}

/** Everything the `/model` sheet lists. Each model is in Recent or in its provider's group, never both. */
export interface ModelSheet {
  /** The current model, then the others chosen before, most recent first. */
  readonly recent: readonly SheetModel[]
  readonly groups: readonly SheetGroup[]
  /** Every provider whose catalog was read, in registration order. */
  readonly providers: readonly { readonly id: string; readonly name: string }[]
  /** Providers whose catalogs could not be read. */
  readonly unavailable: readonly string[]
}

/**
 * Read every provider's models with the facts their rows show.
 *
 * A model whose details cannot be resolved is still listed, without facts;
 * a provider whose catalog cannot be read is named in `unavailable`.
 *
 * @param llm - Harness LLM service.
 * @param current - the session's live selection, which leads Recent even when no catalog lists it.
 * @param recent - routes chosen before, most recent first; routes no catalog lists are dropped.
 * @param signal - stops further reads.
 * @param names - what to call a provider whose adapter names it only by its id, such as its sign-in's label.
 * @returns the sheet's groups.
 */
export async function loadModelSheet(llm: LlmRuntime, current: ModelSelection, recent: readonly string[],
  signal: AbortSignal, names: ReadonlyMap<string, string> = new Map()): Promise<ModelSheet> {
  const groups: SheetGroup[] = []
  const providers: { id: string; name: string }[] = []
  const unavailable: string[] = []
  const selected = routeOf(current)
  for (const provider of llm.listProviders()) {
    signal.throwIfAborted()
    let listed: readonly LlmModelInfo[]
    try {
      listed = await llm.listModels(provider.id)
      signal.throwIfAborted()
    } catch (_providerCatalogUnavailable) {
      signal.throwIfAborted()
      unavailable.push(provider.id)
      continue
    }
    const models = await Promise.all(listed.map(async model => {
      const route = `${provider.id}/${model.id}`
      return sheetModel(provider.id, model, await resolveRoute(llm, route, signal), route === selected)
    }))
    const name = provider.name !== provider.id ? provider.name : names.get(provider.id) ?? provider.name
    providers.push({ id: provider.id, name })
    groups.push({ provider: provider.id, name, models: newestFirst(models) })
  }
  const every = new Map(groups.flatMap(group => group.models.map(model => [model.route, model] as const)))
  const leading = withRecent(recent.filter(route => every.has(route)), selected)
  const recentModels = leading.map(route => every.get(route)
    ?? { route, provider: current.provider, model: current.model, name: current.model, current: true })
  const taken = new Set(leading)
  return {
    recent: recentModels,
    groups: groups.map(group => ({ ...group, models: group.models.filter(model => !taken.has(model.route)) }))
      .filter(group => group.models.length > 0),
    providers,
    unavailable,
  }
}

function sheetModel(provider: string, listed: LlmModelInfo, info: LlmResolvedModelInfo | undefined, current: boolean): SheetModel {
  const modalities = info?.inputModalities ?? listed.inputModalities
  const name = info?.name ?? listed.name
  return {
    route: `${provider}/${listed.id}`, provider, model: listed.id, name: name === '' ? listed.id : name, current,
    ...info?.context === undefined ? {} : { context: info.context.contextWindow },
    ...modalities === undefined ? {} : { image: modalities.includes('image') },
    ...info?.reasoning === undefined ? {} : { reasoning: info.reasoning },
  }
}

/**
 * A version a model's name or id carries, as numbers: `GPT-5.4 mini` is
 * `[5, 4]`, `claude-opus-4-5-20251101` is `[4, 5]`, `o3` is `[3]`. One
 * letter may lead the number, as in `V4` and `K2.6`. A date is not a
 * version: components after the first are one or two digits.
 * @param text - a model's display name or id.
 * @returns the version, or undefined when the text carries none.
 */
export function versionOf(text: string): readonly number[] | undefined {
  const match = /(?:^|[\s(_/-])[a-z]?(\d{1,2})((?:[.-]\d{1,2}(?!\d))*)(?!\d)/iu.exec(text)
  if (match === null) return undefined
  return [Number(match[1]), ...match[2]!.split(/[.-]/u).filter(part => part !== '').map(Number)]
}

/**
 * Models newest first, by the version their name or else their id carries.
 * The sort is stable, so equal versions, such as an alias and its dated
 * snapshot, keep the catalog's order. Models without a version follow, by name.
 * @param models - one provider's models, in catalog order.
 * @returns the same models, reordered.
 */
export function newestFirst<T extends { readonly name: string; readonly model: string }>(models: readonly T[]): readonly T[] {
  const keyed = models.map(model => ({ model, version: versionOf(model.name) ?? versionOf(model.model) }))
  return keyed.sort((left, right) => {
    if (left.version === undefined || right.version === undefined) {
      if (left.version !== right.version) return left.version === undefined ? 1 : -1
      return left.model.name.localeCompare(right.model.name)
    }
    for (let at = 0; at < Math.max(left.version.length, right.version.length); at++) {
      const difference = (right.version[at] ?? -1) - (left.version[at] ?? -1)
      if (difference !== 0) return difference
    }
    return 0
  }).map(entry => entry.model)
}

/**
 * The `/model` sheet: Recent, then each provider's models under its name,
 * with each row's context, reasoning, and image facts, and the selected
 * model's reasoning efforts on the levels row.
 *
 * A model's effort starts where the session's is: its own when it is the
 * current model, the same effort on another model that offers it, and the
 * provider default otherwise.
 *
 * @param sheet - what {@link loadModelSheet} read.
 * @param current - the session's live selection.
 * @param copy - localized labels.
 * @returns a tall picker prompt whose values are routes and whose levels are effort ids, `''` for the default.
 */
export function modelSheetPrompt(sheet: ModelSheet, current: ModelSelection, copy: TuiCopy): ChoicePrompt {
  const names = new Map(sheet.providers.map(provider => [provider.id, provider.name]))
  const choice = (model: SheetModel, group: string, recent: boolean): Choice => {
    const efforts = model.reasoning?.efforts ?? []
    const offers = (effort: string | undefined): effort is string => effort !== undefined && efforts.some(item => item.id === effort)
    const fallback = model.reasoning?.defaultEffort
    const fallbackName = fallback === undefined ? undefined : efforts.find(item => item.id === fallback)?.name ?? fallback
    // Recent mixes providers, so its rows name theirs.
    const detail = [...recent ? [names.get(model.provider) ?? model.provider] : [], ...namesRoute(model.name, model.route) ? [] : [model.model]]
    return {
      value: model.route, label: model.name, group, current: model.current,
      ...detail.length === 0 ? {} : { description: detail.join(' \u00b7 ') },
      facts: [
        model.context === undefined ? '' : formatTokens(model.context),
        efforts.length > 0 ? copy.factThink : '',
        model.image === true ? copy.factImage : '',
      ],
      ...efforts.length === 0 ? {} : { levels: {
        items: [{ value: '', label: fallbackName === undefined ? copy.effortDefault : `${copy.effortDefault} (${fallbackName})` },
          ...efforts.map(item => ({ value: item.id, label: item.name }))],
        initial: offers(current.reasoningEffort) ? current.reasoningEffort : '',
      } },
    }
  }
  const choices = [
    ...sheet.recent.map(model => choice(model, copy.recentModels, true)),
    ...sheet.groups.flatMap(group => group.models.map(model => choice(model, group.name, false))),
  ]
  const from = sheet.providers.map(provider => provider.name).join(copy.listSeparator)
  return {
    title: from === '' ? copy.modelSheet : `${copy.modelSheet} \u00b7 ${choices.length} ${copy.modelsFrom} ${from}`,
    initial: routeOf(current), choices, tall: true,
    factAlign: ['right'], levels: { label: copy.effortLevels, none: copy.noEffort }, help: copy.modelSheetHelp,
    ...sheet.unavailable.length === 0 ? {} : { warning: `${copy.modelCatalogError}: ${sheet.unavailable.join(', ')}` },
  }
}

/**
 * Read capabilities for one exact route. Provider and model ids can contain further slashes.
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

/** A validated selection, or the reason the requested route or effort was refused. */
export type SelectionResult =
  | { readonly kind: 'selected'; readonly selection: ModelSelection; readonly reasoning?: LlmModelReasoningInfo }
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
  }, ...info.reasoning === undefined ? {} : { reasoning: info.reasoning } }
}
