/**
 * Model-facing schema assembly for the tool registry: the wire schemas each
 * presentation mode sends, the generated PTC mode SDK contract, and the two
 * prompt sections a PTC mode scope registers. Everything here is a pure
 * projection of the registry's per-scope view, which the caller supplies
 * through a {@link SchemaPresentationSource}.
 * @module
 */

import type { ScopeKey } from 'bake-scope'
import type { ToolSchema } from 'bake-llm'
import { snapshotJsonValue } from 'bake-util-values'
import type { PromptSection, ToolProviderResult } from 'bake-system-prompt'
import type { PtcRuntime } from 'bake-ptc-runtime'
import type { ToolDefinition, ToolPresentationMode } from './index.ts'
import { RUN_CODE_NAME } from './ptc.ts'
import type { PtcSdkLanguage } from './ptc.ts'
import { renderToolsSdk } from './ts-types.ts'
import type { ToolSdkSchema } from './ts-types.ts'
import { renderToolsSdkPy } from './py-types.ts'

/**
 * The model-facing statement of the `ptc` collapse. Names the consequence
 * (the call fails) and the route (inside the program), because a rule the
 * model can only discover by being denied is one it corrects too late.
 */
const PTC_ONLY_INSTRUCTION = `\`${RUN_CODE_NAME}\` is the only tool you can call directly — a tool call naming any other tool fails. Reach every tool the SDK declares below from inside the program.`

/**
 * Language → SDK-section renderer. The registry looks up the loaded
 * `ctx.ptcRuntime.language` in this table when assembling the `tools:sdk`
 * section under a non-native mode; a runtime whose language is not a key
 * fails the assembly loudly (same idiom as `toolOrder` violations). Adding a
 * new backend language is three parallel edits — a {@link PtcSdkLanguage}
 * member, an entry here, and a `RUN_CODE_FLAVORS` entry in `ptc.ts` for
 * its `run_code` schema strings — plus the renderer function this table points
 * at. The `satisfies` clause pins this table's key set to that union, which
 * the flavor table is checked against too, so any of the three left out is a
 * typecheck failure. What no check reaches is the prose that names the values
 * instead of deriving them: the seam's `bake-ptc-runtime` README, its
 * `PtcRuntime.language` JSDoc, and `docs/subsystems/ptc-runtime.md`,
 * plus this package's own README and the `Config.mode` JSDoc.
 */
const SDK_RENDERERS: Record<string, (schemas: ToolSdkSchema[]) => string> = {
  typescript: renderToolsSdk,
  python: renderToolsSdkPy,
} satisfies Record<PtcSdkLanguage, (schemas: ToolSdkSchema[]) => string>

/** The prompt-section order names this module registers sections under. */
type PresentationSectionOrder = 'PTC_ONLY' | 'TOOLS_SDK'

/**
 * The registry facts schema presentation reads. Each call resolves them at
 * use time, so the source must answer for the calling context.
 */
export interface SchemaPresentationSource {
  /**
   * One scope's visibility-resolved definitions and prompt-order names.
   * @param scope - the viewing scope, or undefined for the global view.
   */
  view(scope?: ScopeKey): {
    readonly visible: ReadonlyMap<string, ToolDefinition>
    readonly knownNames: ReadonlySet<string>
  }
  /**
   * The presentation one scope's agent sees.
   * @param scope - the viewing scope, or undefined for the global view.
   */
  modeFor(scope?: ScopeKey): ToolPresentationMode
  /** The loaded PTC runtime, read without requiring one. */
  peekRuntime(): PtcRuntime | undefined
  /** The system prompt's order for one named section. */
  sectionOrder(name: PresentationSectionOrder): number
}

/**
 * The nearest presentation declared along a scope chain, else the deployment
 * default. Nearest scope wins: a preset's standing declaration covers every
 * agent parented under it, and an agent's own (were one ever declared) would
 * override its preset's. The mode decides what the model SEES, which is
 * exactly the class of fact the chain inherits.
 * @param layers - the scope chain's layers, farthest ancestor first.
 * @param fallback - the deployment default.
 * @returns the resolved presentation mode.
 */
export function nearestPresentationMode(
  layers: readonly { readonly mode: ToolPresentationMode | undefined }[],
  fallback: ToolPresentationMode,
): ToolPresentationMode {
  for (let index = layers.length - 1; index >= 0; index -= 1) {
    const mode = layers[index]?.mode
    if (mode !== undefined) return mode
  }
  return fallback
}

/**
 * The prompt statement of the `ptc` executor collapse, registered wherever
 * {@link sdkSection} is and rendering empty outside an effective `ptc`.
 *
 * Every tool contributes its own guidance section naming its tool, none of
 * them qualify how that tool is reached, and they all render before the SDK.
 * Without this the model reads a catalog of tools it is told to use and no
 * statement that only `run_code` may be called, so it emits a native call,
 * receives `UNKNOWN_TOOL` for a tool the prompt just declared, and concludes
 * the deployment is inconsistent. Its order places the rule before that
 * guidance rather than after it.
 *
 * `both` renders empty: native calls do execute there, so the rule is false.
 * @param source - the registry facts the section renders from.
 * @returns the section registration.
 */
export function collapseSection(source: SchemaPresentationSource): PromptSection {
  return {
    name: 'tools:ptc-only',
    order: source.sectionOrder('PTC_ONLY'),
    // The SAME predicate the executor denies by, so the prompt cannot state
    // a rule the registry does not enforce (see `ToolRuntime.collapses`).
    text: context => source.modeFor(context.scope) === 'ptc' ? PTC_ONLY_INSTRUCTION : '',
  }
}

/**
 * The generated-SDK prompt section, registered globally by a PTC mode
 * deployment and per scope by `ToolRuntime.presentAs`.
 *
 * The body regenerates from the CALLING scope, and renders empty for an
 * agent presenting natively — an agent that opted out under a PTC mode
 * deployment still sees the global registration, and an empty section is
 * dropped from the rendered prompt.
 * @param source - the registry facts the section renders from.
 * @returns the section registration.
 */
export function sdkSection(source: SchemaPresentationSource): PromptSection {
  return {
    name: 'tools:sdk',
    order: source.sectionOrder('TOOLS_SDK'),
    interpolate: false,
    // Regenerate from the calling scope's visible tools in stable order.
    text: (context) => {
      const mode = source.modeFor(context.scope)
      if (mode === 'native') return ''
      const runtime = requirePtcRuntime(source.peekRuntime(), mode)
      // Own-property read: a language like `toString`/`constructor` would
      // otherwise resolve an inherited Object.prototype member as a renderer.
      const render = SDK_RENDERERS[runtime.language]
      /* v8 ignore next -- requirePtcRuntime rejects an unknown language before this runs. */
      if (render === undefined) throw new Error(`dsh-tools: no SDK renderer for ${runtime.language}`)
      return render(sdkSchemas(source.view(context.scope).visible))
    },
  }
}

/**
 * Build one scope's wire schemas and names for prompt-order validation.
 * Restrictions do not make known tools invalid, but a mode collapse does.
 * @param source - the registry facts the schemas project from.
 * @param scope - the viewing scope, or undefined for the global view.
 * @returns the scope's tool-provider contribution.
 */
export function wireSchemas(source: SchemaPresentationSource, scope?: ScopeKey): ToolProviderResult {
  const view = source.view(scope)
  const mode = source.modeFor(scope)
  if (mode === 'native') {
    const schemas = [...view.visible.values()].map(definition => schemaOf(definition, false))
    return { schemas, knownNames: [...view.knownNames] }
  }
  // Validate the runtime language BEFORE projecting schemas: schemaOf reads
  // run_code's language-aware description/parameters getters, whose own
  // flavor-table guard would otherwise surface first. This keeps the
  // renderer-table rejection the canonical assembly-time error for a
  // language with no SDK renderer.
  requirePtcRuntime(source.peekRuntime(), mode)
  const schemas = [...view.visible.values()].map(definition => schemaOf(definition, false))
  if (mode === 'ptc') {
    return {
      schemas: schemas.filter(schema => schema.name === RUN_CODE_NAME),
      knownNames: [RUN_CODE_NAME],
    }
  }
  return { schemas, knownNames: [...view.knownNames, RUN_CODE_NAME] }
}

/**
 * Resolve the PTC runtime or throw the actionable misconfiguration error.
 * Callers read the runtime at use time (assembly / run_code execution), NOT
 * via static `inject`: an inject entry would hold `ctx.tools` — and every
 * tool plugin behind it — hostage to a PTC runtime existing even under
 * `mode: 'native'`.
 *
 * Assembly and `run_code` execution read separately, so the language is not
 * bound to a request. Harmless while one published backend exists — both
 * reads return the same flavor — but a reload that swapped in a second
 * language between them would hand a program written against one SDK to the
 * other. Binding it is deferred until a second backend ships (the first
 * point it is testable).
 * @param runtime - the loaded runtime, if any.
 * @param mode - the presentation that needs it, named in the error.
 * @returns the runtime, whose language has a registered SDK renderer.
 */
export function requirePtcRuntime(runtime: PtcRuntime | undefined, mode: ToolPresentationMode): PtcRuntime {
  if (!runtime) {
    throw new Error(`dsh-tools: mode "${mode}" requires a PTC runtime — load a ctx.ptcRuntime implementation (e.g. @deepseek-ai/dsh-ptc-runtime-codemode) or set tools mode to "native"`)
  }
  if (!Object.hasOwn(SDK_RENDERERS, runtime.language)) {
    const known = Object.keys(SDK_RENDERERS).map(name => JSON.stringify(name)).join(', ')
    throw new Error(`dsh-tools: no SDK renderer registered for runtime language ${JSON.stringify(runtime.language)} (known: ${known})`)
  }
  return runtime
}

/**
 * Project visible callable tools onto the generated PTC mode SDK contract.
 * @param visible - one scope's visible definitions in stable order.
 * @returns one SDK binding schema per callable tool.
 */
export function sdkSchemas(visible: ReadonlyMap<string, ToolDefinition>): ToolSdkSchema[] {
  return [...visible.values()]
    .filter(definition => definition.name !== RUN_CODE_NAME)
    .map((definition): ToolSdkSchema => {
      const output = snapshotJsonValue(definition.output.schema)
      /* v8 ignore next -- registration already validated and retained this schema as lossless JSON. */
      if (output === undefined) {
        throw new Error(`tool "${definition.name}" output schema must be lossless JSON before SDK projection`)
      }
      return { ...schemaOf(definition, true), output }
    })
}

/**
 * Project one definition onto the model-facing schema fields.
 * @param definition - the registered definition.
 * @param detachParameters - whether to snapshot the parameters instead of sharing them.
 * @returns the name, description, and parameters only.
 */
export function schemaOf(definition: ToolDefinition, detachParameters: boolean): ToolSchema {
  const { name, description, parameters } = definition
  const detached = detachParameters ? snapshotJsonValue(parameters) : parameters
  if (detached === undefined) {
    throw new Error(`tool "${name}" parameters must be lossless JSON before schema projection`)
  }
  return {
    name,
    description,
    parameters: detached,
  }
}
