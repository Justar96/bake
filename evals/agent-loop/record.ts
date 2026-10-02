/**
 * Turn raw paired-eval output into a committed version record: compact
 * per-sample metrics, a machine-readable summary with paired comparisons, and
 * a Markdown report. See evals/README.md.
 *
 * bun evals/agent-loop/record.ts --raw <dir>[,<dir>...] --out <record dir>
 *   --arm <arm>=<version label>[,...]   every arm the raw samples carry, mapped to the version it measured
 *   --candidate <arm>                   the arm this record is about
 *   [--base <arm>[,<arm>...]]           arms compared against it, from the same run; omit for a baseline-only record
 *   [--title <text>] [--note <text>] [--harness <path>] [--replace] [--fail-on-regression]
 */
import { mkdirSync, readFileSync, readdirSync, statSync, writeFileSync, existsSync } from 'node:fs'
import { dirname, join, relative, resolve } from 'node:path'

type Sample = Record<string, any>

const args = new Map<string, string>()
const flags = new Set<string>()
for (let index = 2; index < process.argv.length; index++) {
  const key = process.argv[index]!
  if (!key.startsWith('--')) throw new Error(`unexpected argument ${key}`)
  const next = process.argv[index + 1]
  if (next === undefined || next.startsWith('--')) flags.add(key.slice(2))
  else { args.set(key.slice(2), next); index++ }
}
const required = (name: string) => {
  const value = args.get(name)
  if (value === undefined) throw new Error(`--${name} is required`)
  return value
}
const rawDirs = required('raw').split(',')
const outDir = required('out')
const armLabels = Object.fromEntries(required('arm').split(',').map(entry => entry.split('=') as [string, string]))
const candidate = required('candidate')
const bases = (args.get('base') ?? '').split(',').filter(Boolean)

/** Raw sample files: `<model>.<scenario>.<trial>.<arm>.json`, one level of model subdirectories allowed. */
function rawSamples(dir: string): Sample[] {
  const found: Sample[] = []
  for (const entry of readdirSync(dir)) {
    const path = join(dir, entry)
    if (statSync(path).isDirectory()) { found.push(...rawSamples(path)); continue }
    if (!entry.endsWith('.json') || entry.includes('.request') || entry === 'results.json' || entry === 'design.json' || entry === 'report.json') continue
    const sample = JSON.parse(readFileSync(path, 'utf8'))
    if (typeof sample.label === 'string' && sample.variant in armLabels) found.push(sample)
  }
  return found
}
const designs = rawDirs.flatMap(dir => {
  const found: Sample[] = []
  const walk = (current: string) => {
    for (const entry of readdirSync(current)) {
      const path = join(current, entry)
      if (statSync(path).isDirectory()) walk(path)
      else if (entry === 'design.json') found.push(JSON.parse(readFileSync(path, 'utf8')))
    }
  }
  walk(dir)
  return found
})

/** The committed shape: counters and classifications only, no transcripts, file contents, or stderr. */
function compact(sample: Sample) {
  const usage = sample.usage ?? {}
  const first = sample.requestMetrics?.[0] ?? {}
  return {
    model: sample.model, scenario: sample.scenario, trial: sample.trial, arm: sample.variant, version: armLabels[sample.variant],
    success: sample.success === true, usageComplete: sample.usageComplete === true,
    failure: sample.success === true ? null
      : sample.abortCause ?? (sample.code !== 0 ? `exit ${sample.code}` : (sample.final ?? '') === '' ? 'empty final reply' : 'validation failed'),
    requests: sample.requests, toolCalls: sample.toolCalls, toolErrors: sample.toolErrors, byTool: sample.byTool ?? {},
    guardRefusals: sample.guardRefusals ?? null, shellEdits: sample.shellEdits ?? null, multiEditCalls: sample.multiEditCalls ?? null,
    toolHelpCalls: sample.toolHelpCalls ?? null, workflowCalls: sample.workflowCalls ?? null,
    inputTokens: usage.inputTokens ?? 0, cacheReadTokens: usage.cacheReadTokens ?? 0, cacheWriteTokens: usage.cacheWriteTokens ?? 0,
    outputTokens: usage.outputTokens ?? 0, totalTokens: sample.logicalTotalTokens ?? usage.totalTokens ?? 0,
    firstRequestBytes: first.requestBytes ?? null, toolSchemaChars: first.toolSchemaChars ?? null,
    elapsedMs: sample.elapsedMs ?? null,
    toolErrorMessages: (sample.toolErrorMessages ?? []).map((text: string) => String(text).split('\n')[0]!.slice(0, 160)),
    subagentCalls: sample.subagentCalls ?? null,
    routingDecisions: sample.routingDecisions ?? null,
  }
}
type Compact = ReturnType<typeof compact>

const samples = rawDirs.flatMap(rawSamples).map(compact)
  .sort((a, b) => `${a.model}.${a.scenario}.${a.trial}.${a.arm}`.localeCompare(`${b.model}.${b.scenario}.${b.trial}.${b.arm}`))
if (!samples.some(sample => sample.arm === candidate)) throw new Error(`no samples for candidate arm ${candidate}`)
const models = [...new Set(samples.map(sample => sample.model))].sort()

/** Seeded generator so a re-recorded run reproduces its intervals. */
function mulberry32(seed: number) {
  return () => {
    seed |= 0; seed = seed + 0x6D2B79F5 | 0
    let t = Math.imul(seed ^ seed >>> 15, 1 | seed)
    t = t + Math.imul(t ^ t >>> 7, 61 | t) ^ t
    return ((t ^ t >>> 14) >>> 0) / 4294967296
  }
}
/** Percent change of the summed metric, candidate over base, with a paired bootstrap 95% interval. */
function pairedChange(pairs: [Compact, Compact][], metric: (sample: Compact) => number) {
  const base = pairs.map(pair => metric(pair[0])); const next = pairs.map(pair => metric(pair[1]))
  const sum = (values: number[]) => values.reduce((total, value) => total + value, 0)
  if (sum(base) === 0) return null
  const random = mulberry32(7)
  const draws: number[] = []
  for (let round = 0; round < 4000; round++) {
    let b = 0; let c = 0
    for (let index = 0; index < pairs.length; index++) { const pick = Math.floor(random() * pairs.length); b += base[pick]!; c += next[pick]! }
    if (b > 0) draws.push((c - b) / b * 100)
  }
  draws.sort((x, y) => x - y)
  const round1 = (value: number) => Math.round(value * 10) / 10
  return { pct: round1((sum(next) - sum(base)) / sum(base) * 100), lo: round1(draws[Math.floor(0.025 * draws.length)]!), hi: round1(draws[Math.ceil(0.975 * draws.length) - 1]!) }
}
const uncached = (sample: Compact) => sample.inputTokens + sample.cacheWriteTokens
const logicalInput = (sample: Compact) => sample.inputTokens + sample.cacheWriteTokens + sample.cacheReadTokens

function compare(model: string, baseArm: string, scenarios: (scenario: string) => boolean) {
  const key = (sample: Compact) => `${sample.scenario}.${sample.trial}`
  const baseByKey = new Map(samples.filter(s => s.model === model && s.arm === baseArm && scenarios(s.scenario)).map(s => [key(s), s]))
  const matched = samples.filter(s => s.model === model && s.arm === candidate && scenarios(s.scenario) && baseByKey.has(key(s)))
    .map(s => [baseByKey.get(key(s))!, s] as [Compact, Compact])
  const usable = matched.filter(([b, c]) => b.success && c.success && b.usageComplete && c.usageComplete)
  const total = (arm: 0 | 1, metric: (sample: Compact) => number) => usable.reduce((sum, pair) => sum + metric(pair[arm]), 0)
  return {
    cells: matched.length, pairs: usable.length,
    failures: [matched.filter(pair => !pair[0].success).length, matched.filter(pair => !pair[1].success).length],
    totalTokens: usable.length ? pairedChange(usable, s => s.totalTokens) : null,
    logicalInputTokens: usable.length ? pairedChange(usable, logicalInput) : null,
    uncachedInputTokens: usable.length ? pairedChange(usable, uncached) : null,
    requests: [total(0, s => s.requests), total(1, s => s.requests)],
    toolCalls: [total(0, s => s.toolCalls), total(1, s => s.toolCalls)],
    toolErrors: [total(0, s => s.toolErrors), total(1, s => s.toolErrors)],
  }
}
type Comparison = ReturnType<typeof compare>

const scenarioSet = [...new Set(samples.map(sample => sample.scenario))].sort()
const groups: Record<string, (scenario: string) => boolean> = {
  'all tasks': scenario => scenario !== 'no_tools',
  ...Object.fromEntries(scenarioSet.filter(scenario => scenario !== 'no_tools').map(scenario => [scenario, (s: string) => s === scenario])),
}

/**
 * Regression rule: over all tasks for one model, a total-token increase whose
 * whole 95% interval is above zero, more failed runs than the base by two or
 * more, or more tool errors than the base.
 */
function regressions(model: string, baseArm: string, result: Comparison): string[] {
  const found: string[] = []
  const label = `${model} vs ${armLabels[baseArm]}`
  if (result.totalTokens !== null && result.totalTokens.lo > 0) found.push(`${label}: total tokens ${result.totalTokens.pct}% [${result.totalTokens.lo}, ${result.totalTokens.hi}]`)
  if (result.failures[1]! >= result.failures[0]! + 2) found.push(`${label}: failures ${result.failures[0]} -> ${result.failures[1]}`)
  if (result.toolErrors[1]! > result.toolErrors[0]!) found.push(`${label}: tool errors ${result.toolErrors[0]} -> ${result.toolErrors[1]}`)
  return found
}

const perModel: Record<string, any> = {}
const flagged: string[] = []
for (const model of models) {
  const own = samples.filter(sample => sample.model === model && sample.arm === candidate)
  const ok = own.filter(sample => sample.success && sample.scenario !== 'no_tools')
  const control = own.filter(sample => sample.scenario === 'no_tools' && sample.firstRequestBytes !== null)
  const mean = (values: number[]) => values.length === 0 ? null : Math.round(values.reduce((a, b) => a + b, 0) / values.length)
  perModel[model] = {
    samples: own.length, successes: own.filter(sample => sample.success).length,
    firstRequestBytes: control[0]?.firstRequestBytes ?? null, toolSchemaChars: control[0]?.toolSchemaChars ?? null,
    meanTotalTokensPerTask: mean(ok.map(sample => sample.totalTokens)),
    meanRequestsPerTask: ok.length === 0 ? null : Math.round(ok.reduce((sum, s) => sum + s.requests, 0) / ok.length * 100) / 100,
    cacheReadShare: (() => {
      const input = ok.reduce((sum, s) => sum + logicalInput(s), 0)
      return input === 0 ? null : Math.round(ok.reduce((sum, s) => sum + s.cacheReadTokens, 0) / input * 1000) / 1000
    })(),
    toolErrors: own.reduce((sum, s) => sum + s.toolErrors, 0),
    comparisons: Object.fromEntries(bases.map(baseArm => {
      const results = Object.fromEntries(Object.entries(groups).map(([name, filter]) => [name, compare(model, baseArm, filter)]))
      flagged.push(...regressions(model, baseArm, results['all tasks']!))
      return [armLabels[baseArm], results]
    })),
  }
}

const design = designs[0] ?? {}
const summary = {
  title: args.get('title') ?? `${armLabels[candidate]} agent-loop eval`,
  version: armLabels[candidate],
  arms: Object.fromEntries(Object.entries(armLabels).map(([arm, label]) => [arm, {
    version: label, revision: designs.map(d => d.revisions?.[arm]).find(Boolean) ?? null,
  }])),
  candidate, bases: bases.map(arm => armLabels[arm]),
  models, scenarios: scenarioSet, trials: Math.max(...samples.map(sample => sample.trial)) + 1,
  startedAt: designs.map(d => d.startedAt).filter(Boolean).sort()[0] ?? null,
  harness: args.get('harness') ?? 'evals/agent-loop/run.ts',
  note: args.get('note') ?? null,
  settings: { effort: design.effort ?? null, maxOutputTokens: design.maxOutputTokens ?? null, capMs: design.capMs ?? null, maxRequests: design.maxRequests ?? null },
  regressionRule: 'all tasks, per model: total tokens with the 95% interval above zero; failures +2 or more; any tool-error increase',
  regressions: flagged,
  perModel,
}

const fmt = (change: { pct: number; lo: number; hi: number } | null) => change === null ? 'n/a' : `${change.pct > 0 ? '+' : ''}${change.pct}% [${change.lo}, ${change.hi}]`
const lines: string[] = [
  `# ${summary.title}`, '',
  `Candidate \`${summary.version}\`${summary.arms[candidate]?.revision?.commit ? ` at \`${String(summary.arms[candidate].revision.commit).slice(0, 10)}\`` : ''}${bases.length === 0 ? ', measured on its own as a baseline.' : `, compared with ${bases.map(arm => `\`${armLabels[arm]}\``).join(' and ')} in the same run.`}`,
  `Models: ${models.join(', ')}. Scenarios: ${scenarioSet.join(', ')}. Trials: ${summary.trials}. Started: ${summary.startedAt ?? 'unknown'}.`,
  ...summary.note ? ['', summary.note] : [],
  '', 'Token changes are the candidate\'s summed total over the base\'s, over pairs where both runs succeeded, with a paired bootstrap 95% interval.', '',
  '## Regressions', '',
  ...bases.length === 0 ? ['Not assessed: a baseline record has nothing to compare against.']
    : flagged.length === 0 ? [`None under the rule in [evals/README.md](${relative(resolve(outDir), resolve(dirname(import.meta.path), '../README.md'))}#regressions).`]
      : flagged.map(text => `- ${text}`),
]
for (const baseArm of bases) {
  lines.push('', `## Against \`${armLabels[baseArm]}\``, '', '| Model | Pairs | Total tokens | Uncached input | Requests | Tool errors | Failures |', '|---|---|---|---|---|---|---|')
  for (const model of models) {
    const result = perModel[model].comparisons[armLabels[baseArm]]['all tasks'] as Comparison
    lines.push(`| ${model} | ${result.pairs}/${result.cells} | ${fmt(result.totalTokens)} | ${fmt(result.uncachedInputTokens)} | ${result.requests[0]} → ${result.requests[1]} | ${result.toolErrors[0]} → ${result.toolErrors[1]} | ${result.failures[0]} → ${result.failures[1]} |`)
  }
  lines.push('', '<details><summary>Per scenario</summary>', '', '| Model | Scenario | Pairs | Total tokens | Requests | Failures |', '|---|---|---|---|---|---|')
  for (const model of models) for (const scenario of Object.keys(groups).filter(name => name !== 'all tasks')) {
    const result = perModel[model].comparisons[armLabels[baseArm]][scenario] as Comparison
    if (result.cells === 0) continue
    lines.push(`| ${model} | ${scenario} | ${result.pairs}/${result.cells} | ${fmt(result.totalTokens)} | ${result.requests[0]} → ${result.requests[1]} | ${result.failures[0]} → ${result.failures[1]} |`)
  }
  lines.push('', '</details>')
}
lines.push('', `## \`${summary.version}\` on its own`, '', '| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |', '|---|---|---|---|---|---|---|')
for (const model of models) {
  const own = perModel[model]
  lines.push(`| ${model} | ${own.successes}/${own.samples} | ${own.firstRequestBytes ?? 'n/a'} | ${own.meanTotalTokensPerTask ?? 'n/a'} | ${own.meanRequestsPerTask ?? 'n/a'} | ${own.cacheReadShare === null ? 'n/a' : `${Math.round(own.cacheReadShare * 100)}%`} | ${own.toolErrors} |`)
}
const failed = samples.filter(sample => !sample.success)
if (failed.length > 0) {
  lines.push('', '## Failed runs', '', '| Model | Scenario | Trial | Version | Failure |', '|---|---|---|---|---|')
  for (const sample of failed) lines.push(`| ${sample.model} | ${sample.scenario} | ${sample.trial} | ${sample.version} | ${sample.failure} |`)
}

mkdirSync(outDir, { recursive: true })
if (existsSync(join(outDir, 'summary.json')) && !flags.has('replace')) throw new Error(`${outDir} already holds a record; pass --replace to overwrite it`)
writeFileSync(join(outDir, 'samples.jsonl'), samples.map(sample => JSON.stringify(sample)).join('\n') + '\n')
writeFileSync(join(outDir, 'summary.json'), JSON.stringify(summary, null, 2) + '\n')
writeFileSync(join(outDir, 'REPORT.md'), lines.join('\n') + '\n')
console.log(`recorded ${samples.length} samples in ${outDir}; regressions: ${flagged.length === 0 ? 'none' : flagged.join('; ')}`)
if (flags.has('fail-on-regression') && flagged.length > 0) process.exit(1)
