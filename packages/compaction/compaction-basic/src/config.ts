/**
 * Load-time validation and routed-model policy resolution for compaction-basic.
 *
 * @module @deepseek-ai/dsh-compaction-basic/config
 */

import type { LlmCallConfig } from '@deepseek-ai/dsh-llm'
import { deepFreeze } from '@deepseek-ai/dsh-util-values'
import type {
  BasicCompactionConfig,
  CompactionPolicyConfig,
  ModelCompactPolicyConfig,
  ResolvedCompactSpec,
  ResolvedConfig,
  ResolvedRetention,
  ResolvedTargetPolicy,
  ResolvedThreshold,
} from './types.ts'

/** Default request-pressure fraction for every routed model. */
const DEFAULT_THRESHOLD_RATIO = 0.8

/** Default verbatim-tail fraction for every routed model. */
const DEFAULT_RETAIN_RATIO = 0.16

/** Fields shared by top-level defaults and per-route overrides. */
const POLICY_CONFIG_KEYS = [
  'thresholdRatio',
  'thresholdTokens',
  'retainRatio',
  'retainTokens',
  'summarizationProvider',
  'summarizationModel',
  'maxTokens',
  'compactionRetries',
  'maxOverflowRetries',
] as const

/** Complete public top-level configuration key set. */
const BASIC_COMPACT_CONFIG_KEYS: ReadonlySet<string> = new Set([
  ...POLICY_CONFIG_KEYS,
  'modelPolicies',
  'auto',
])

/** Complete per-route override key set. */
const MODEL_POLICY_KEYS: ReadonlySet<string> = new Set([
  'provider',
  'model',
  ...POLICY_CONFIG_KEYS,
])

/** Target-specific pressure configuration failure eligible for warning suppression. */
export class TargetPressureConfigError extends Error {
  /**
   * @param targetKey - exact provider/model route used as the warning key.
   * @param message - actionable configuration failure detail.
   */
  constructor(readonly targetKey: string, message: string) {
    super(message)
  }
}

/**
 * Resolve and validate service defaults plus per-route partial overrides.
 * Every capacity-independent conflict an override can produce once layered
 * over the defaults (and, for an exact entry, over its provider-wide entry)
 * fails here rather than at the first pressure check.
 * @param config - untrusted plugin configuration or settings section.
 * @param name - diagnostic prefix naming where the configuration came from.
 * @returns detached immutable defaults and validated per-route overrides.
 */
export function resolveConfig(
  config: BasicCompactionConfig = {},
  name = 'BasicCompactionConfig',
): ResolvedConfig {
  validateKeys(config, BASIC_COMPACT_CONFIG_KEYS, name)
  validatePolicy(config, name)
  if (config.auto !== undefined && typeof config.auto !== 'boolean') {
    throw new Error(`${name}: auto must be a boolean`)
  }

  const threshold = resolveThreshold(config, { thresholdRatio: DEFAULT_THRESHOLD_RATIO })
  const retention = resolveRetention(config, { retainRatio: DEFAULT_RETAIN_RATIO })
  validateThresholdRetention(threshold, retention, name)
  const modelPolicies = resolveModelPolicies(config.modelPolicies, name)
  for (const [index, policy] of modelPolicies.entries()) {
    const providerWide = policy.model === undefined
      ? undefined
      : modelPolicies.find(other => other.provider === policy.provider && other.model === undefined)
    const inheritedThreshold = resolveThreshold(providerWide ?? {}, threshold)
    const inheritedRetention = resolveRetention(providerWide ?? {}, retention)
    validateThresholdRetention(
      resolveThreshold(policy, inheritedThreshold),
      resolveRetention(policy, inheritedRetention),
      `${name}: modelPolicies[${index}]`,
    )
  }

  return deepFreeze({
    ...threshold,
    ...retention,
    summarizationProvider: config.summarizationProvider ?? '',
    summarizationModel: config.summarizationModel ?? '',
    maxTokens: config.maxTokens ?? 8192,
    compactionRetries: config.compactionRetries ?? 1,
    maxOverflowRetries: config.maxOverflowRetries ?? 1,
    modelPolicies,
    auto: config.auto ?? true,
  })
}

/**
 * Layer the matching overrides over the validated default policy, field by
 * field: the exact provider/model entry over the provider-wide entry over the
 * defaults. A threshold or retention form set at a more specific level
 * replaces the inherited form as a unit.
 * @param config - validated service defaults and override table.
 * @param target - exact durable provider/model route to match.
 * @returns detached immutable policy before model-capacity scaling.
 */
export function resolveTargetPolicy(
  config: ResolvedConfig,
  target: Pick<LlmCallConfig, 'provider' | 'model'>,
): ResolvedTargetPolicy {
  const providerWide = config.modelPolicies.find(policy => (
    policy.provider === target.provider && policy.model === undefined
  ))
  const exact = config.modelPolicies.find(policy => (
    policy.provider === target.provider && policy.model === target.model
  ))
  const defaultThreshold: ResolvedThreshold = config.thresholdTokens === undefined
    ? { thresholdRatio: config.thresholdRatio }
    : { thresholdTokens: config.thresholdTokens }
  const defaultRetention: ResolvedRetention = config.retainTokens === undefined
    ? { retainRatio: config.retainRatio }
    : { retainTokens: config.retainTokens }
  const pick = <K extends keyof CompactionPolicyConfig>(
    key: K,
    fallback: NonNullable<CompactionPolicyConfig[K]>,
  ): NonNullable<CompactionPolicyConfig[K]> => exact?.[key] ?? providerWide?.[key] ?? fallback
  return deepFreeze({
    target: { provider: target.provider, model: target.model },
    ...resolveThreshold(exact ?? {}, resolveThreshold(providerWide ?? {}, defaultThreshold)),
    ...resolveRetention(exact ?? {}, resolveRetention(providerWide ?? {}, defaultRetention)),
    summarizationProvider: pick('summarizationProvider', config.summarizationProvider),
    summarizationModel: pick('summarizationModel', config.summarizationModel),
    maxTokens: pick('maxTokens', config.maxTokens),
    compactionRetries: pick('compactionRetries', config.compactionRetries),
    maxOverflowRetries: pick('maxOverflowRetries', config.maxOverflowRetries),
  })
}

/**
 * Scale one routed policy into concrete token budgets for its model capacity.
 * @param policy - merged policy for the exact routed target.
 * @param contextWindow - positive adapter-owned capacity for that target.
 * @returns detached immutable pressure and retention budgets.
 */
export function resolveCompactSpec(
  policy: ResolvedTargetPolicy,
  contextWindow: number,
): ResolvedCompactSpec {
  const targetKey = `${policy.target.provider}/${policy.target.model}`
  if (!Number.isInteger(contextWindow) || contextWindow <= 0) {
    throw new TargetPressureConfigError(
      targetKey,
      `BasicCompactionConfig: contextWindow (${contextWindow}) must be a positive integer`,
    )
  }
  const thresholdTokens = policy.thresholdTokens ?? Math.floor(contextWindow * policy.thresholdRatio)
  if (thresholdTokens > contextWindow) {
    throw new TargetPressureConfigError(
      targetKey,
      `BasicCompactionConfig: ${targetKey} thresholdTokens (${thresholdTokens}) exceeds `
      + `the model's contextWindow (${contextWindow}); lower it or use thresholdRatio`,
    )
  }
  const retainTokens = policy.retainTokens === undefined
    ? Math.floor(contextWindow * policy.retainRatio)
    : policy.retainTokens
  if (retainTokens >= thresholdTokens) {
    throw new TargetPressureConfigError(
      targetKey,
      `BasicCompactionConfig: ${policy.target.provider}/${policy.target.model} retainTokens `
      + `(${retainTokens}) must be less than threshold tokens ${thresholdTokens}`,
    )
  }
  return deepFreeze({
    target: { ...policy.target },
    contextWindow,
    thresholdTokens,
    retainTokens,
    summarizationProvider: policy.summarizationProvider,
    summarizationModel: policy.summarizationModel,
    maxTokens: policy.maxTokens,
    compactionRetries: policy.compactionRetries,
    maxOverflowRetries: policy.maxOverflowRetries,
  })
}

/** Choose an explicit retention form or inherit the already-resolved fallback. */
function resolveRetention(
  config: CompactionPolicyConfig,
  fallback: ResolvedRetention,
): ResolvedRetention {
  if (config.retainTokens !== undefined) return { retainTokens: config.retainTokens }
  if (config.retainRatio !== undefined) return { retainRatio: config.retainRatio }
  return fallback
}

/** Choose an explicit threshold form or inherit the already-resolved fallback. */
function resolveThreshold(
  config: CompactionPolicyConfig,
  fallback: ResolvedThreshold,
): ResolvedThreshold {
  if (config.thresholdTokens !== undefined) return { thresholdTokens: config.thresholdTokens }
  if (config.thresholdRatio !== undefined) return { thresholdRatio: config.thresholdRatio }
  return fallback
}

/**
 * Reject a capacity-independent threshold/retention conflict at load: two
 * ratios, or two absolute budgets. A mixed pair depends on the routed model's
 * window and is judged per target by {@link resolveCompactSpec}.
 */
function validateThresholdRetention(
  threshold: ResolvedThreshold,
  retention: ResolvedRetention,
  name: string,
): void {
  if (threshold.thresholdRatio !== undefined && retention.retainRatio !== undefined
    && retention.retainRatio >= threshold.thresholdRatio) {
    throw new Error(
      `${name}: retainRatio (${retention.retainRatio}) must be less than `
      + `the resolved thresholdRatio (${threshold.thresholdRatio})`,
    )
  }
  if (threshold.thresholdTokens !== undefined && retention.retainTokens !== undefined
    && retention.retainTokens >= threshold.thresholdTokens) {
    throw new Error(
      `${name}: retainTokens (${retention.retainTokens}) must be less than `
      + `the resolved thresholdTokens (${threshold.thresholdTokens})`,
    )
  }
}

/** Validate, detach, and reject duplicate exact and provider-wide policies. */
function resolveModelPolicies(configured: unknown, owner: string): ModelCompactPolicyConfig[] {
  if (configured === undefined) return []
  if (!Array.isArray(configured)) {
    throw new Error(`${owner}: modelPolicies must be an array`)
  }
  const seen = new Set<string>()
  return configured.map((source: unknown, index) => {
    const name = `${owner}: modelPolicies[${index}]`
    assertModelPolicy(source, name)
    const key = source.model === undefined
      ? `${source.provider}\u0000`
      : `${source.provider}\u0000\u0000${source.model}`
    if (seen.has(key)) {
      throw new Error(source.model === undefined
        ? `${owner}: duplicate provider-wide model policy for ${source.provider}`
        : `${owner}: duplicate model policy for ${source.provider}/${source.model}`)
    }
    seen.add(key)
    return { ...source }
  })
}

/** Validate one untrusted per-route override and narrow its public type. */
function assertModelPolicy(
  source: unknown,
  name: string,
): asserts source is ModelCompactPolicyConfig {
  if (!isUnknownRecord(source)) throw new Error(`${name} must be an object`)
  validateKeys(source, MODEL_POLICY_KEYS, name)
  assertNonEmptyString(`${name}.provider`, source.provider)
  if (source.model !== undefined) assertNonEmptyString(`${name}.model`, source.model)
  validatePolicy(source, name)
}

/** Validate the fields common to defaults and per-route partial overrides. */
function validatePolicy(
  config: CompactionPolicyConfig | Record<string, unknown>,
  name: string,
): void {
  const thresholdRatio = config.thresholdRatio
  const thresholdTokens = config.thresholdTokens
  const retainRatio = config.retainRatio
  const retainTokens = config.retainTokens
  const maxTokens = config.maxTokens
  const compactionRetries = config.compactionRetries
  const maxOverflowRetries = config.maxOverflowRetries
  if (thresholdRatio !== undefined) assertRatio(`${name}.thresholdRatio`, thresholdRatio)
  if (thresholdTokens !== undefined) assertPositiveInteger(`${name}.thresholdTokens`, thresholdTokens)
  if (thresholdRatio !== undefined && thresholdTokens !== undefined) {
    throw new Error(`${name}: thresholdRatio and thresholdTokens are mutually exclusive`)
  }
  if (retainRatio !== undefined) assertRatio(`${name}.retainRatio`, retainRatio)
  if (retainTokens !== undefined) assertNonNegativeInteger(`${name}.retainTokens`, retainTokens)
  if (retainRatio !== undefined && retainTokens !== undefined) {
    throw new Error(`${name}: retainRatio and retainTokens are mutually exclusive`)
  }
  if (maxTokens !== undefined) assertPositiveInteger(`${name}.maxTokens`, maxTokens)
  if (compactionRetries !== undefined) {
    assertNonNegativeInteger(`${name}.compactionRetries`, compactionRetries)
  }
  if (maxOverflowRetries !== undefined) {
    assertNonNegativeInteger(`${name}.maxOverflowRetries`, maxOverflowRetries)
  }

  validateSummarizationPair(config, name)
}

/** Require one scope to omit, clear, or replace the summarization target as a pair. */
function validateSummarizationPair(
  config: CompactionPolicyConfig | Record<string, unknown>,
  name: string,
): void {
  const provider = config.summarizationProvider
  const model = config.summarizationModel
  if (provider !== undefined && typeof provider !== 'string') {
    throw new Error(`${name}.summarizationProvider must be a string`)
  }
  if (model !== undefined && typeof model !== 'string') {
    throw new Error(`${name}.summarizationModel must be a string`)
  }
  if (provider === undefined && model === undefined) return
  if (provider === undefined || model === undefined
    || (provider.length === 0) !== (model.length === 0)) {
    throw new Error(
      `${name}: summarizationProvider and summarizationModel must be set together `
      + 'as an empty or non-empty pair',
    )
  }
}

/** Reject stale or misspelled keys before defaults can hide them. */
function validateKeys(config: object, keys: ReadonlySet<string>, name: string): void {
  for (const key of Object.keys(config)) {
    if (!keys.has(key)) throw new Error(`${name}: unknown key "${key}"`)
  }
}

function isUnknownRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function assertNonEmptyString(name: string, value: unknown): asserts value is string {
  if (typeof value !== 'string' || value.length === 0) {
    throw new Error(`${name} must be a non-empty string`)
  }
}

function assertPositiveInteger(name: string, value: unknown): asserts value is number {
  if (typeof value !== 'number' || !Number.isInteger(value) || value <= 0) {
    throw new Error(`${name} (${String(value)}) must be a positive integer`)
  }
}

function assertNonNegativeInteger(name: string, value: unknown): asserts value is number {
  if (typeof value !== 'number' || !Number.isInteger(value) || value < 0) {
    throw new Error(`${name} (${String(value)}) must be a non-negative integer`)
  }
}

function assertRatio(name: string, value: unknown): asserts value is number {
  if (typeof value !== 'number' || !Number.isFinite(value) || value <= 0 || value > 1) {
    throw new Error(`${name} (${String(value)}) must be a number in (0, 1]`)
  }
}
