#!/usr/bin/env bun
/**
 * Append a CI job's timings to its GitHub step summary: each preflight step's
 * outcome and seconds from `.preflight/summary.json`, and the slowest tests
 * from nextest's JUnit report when the job wrote one.
 *
 *   bun scripts/ci-step-summary.ts --title <job name>
 *
 * Writes to `$GITHUB_STEP_SUMMARY` when it is set, else to stdout. A missing
 * input is noted, never an error, so the summary runs after any failure.
 */
import { appendFileSync, existsSync, readFileSync } from 'node:fs'
import { join, resolve } from 'node:path'

const ROOT = resolve(import.meta.dirname, '..')
const SUMMARY = join(ROOT, '.preflight', 'summary.json')
const JUNIT = join(ROOT, 'rust', 'target', 'nextest', 'ci', 'junit.xml')
const SLOWEST = 20

/** One preflight step as `.preflight/summary.json` records it. */
export interface StepTime { readonly step: string; readonly outcome: string; readonly seconds: number }

/** One JUnit test case that ran. */
export interface TestTime { readonly name: string; readonly suite: string; readonly seconds: number; readonly failed: boolean }

/** The steps in a preflight summary; malformed entries are left out. */
export function parsePreflightSummary(text: string): StepTime[] {
  const parsed: unknown = JSON.parse(text)
  const steps = typeof parsed === 'object' && parsed !== null && 'steps' in parsed ? parsed.steps : undefined
  if (!Array.isArray(steps)) return []
  return steps.flatMap((entry: unknown): StepTime[] => {
    if (typeof entry !== 'object' || entry === null) return []
    const { step, outcome, seconds } = entry as Record<string, unknown>
    return typeof step === 'string' && typeof outcome === 'string' && typeof seconds === 'number' ? [{ step, outcome, seconds }] : []
  })
}

function unescapeXml(value: string): string {
  return value.replace(/&(lt|gt|quot|apos|amp|#\d+|#x[0-9a-f]+);/giu, (whole, entity: string) => {
    const named: Record<string, string> = { lt: '<', gt: '>', quot: '"', apos: "'", amp: '&' }
    if (entity in named) return named[entity] ?? whole
    const code = entity.startsWith('#x') || entity.startsWith('#X') ? Number.parseInt(entity.slice(2), 16) : Number.parseInt(entity.slice(1), 10)
    return Number.isInteger(code) && code >= 0 && code <= 0x10ffff ? String.fromCodePoint(code) : whole
  })
}

function attributes(tag: string): Map<string, string> {
  return new Map([...tag.matchAll(/([\w:-]+)="([^"]*)"/gu)].map(([, key, value]) => [key ?? '', unescapeXml(value ?? '')]))
}

/** Test cases that ran, from nextest's JUnit report; skipped cases carry no time worth ranking. */
export function parseJunit(xml: string): TestTime[] {
  const tests: TestTime[] = []
  // A case is self-closing, or holds children up to its closing tag.
  for (const match of xml.matchAll(/<testcase\b([^>]*?)(\/>|>([\s\S]*?)<\/testcase>)/gu)) {
    const body = match[3] ?? ''
    if (/<skipped\b/u.test(body)) continue
    const attrs = attributes(match[1] ?? '')
    const seconds = Number(attrs.get('time'))
    if (!Number.isFinite(seconds)) continue
    tests.push({ name: attrs.get('name') ?? '', suite: attrs.get('classname') ?? '', seconds, failed: /<(failure|error)\b/u.test(body) })
  }
  return tests
}

// GitHub renders the summary as Markdown with inline HTML, so markup in a name is escaped too.
const cell = (value: string) => value.replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;')
  .replaceAll('|', '\\|').replaceAll('\n', ' ')

/** The Markdown section for one job. */
export function renderStepSummary(title: string, steps: readonly StepTime[] | undefined, tests: readonly TestTime[] | undefined): string {
  const lines = [`### ${cell(title)} timings`, '']
  if (steps === undefined || steps.length === 0) lines.push('No preflight summary was written.', '')
  else {
    const run = steps.filter(step => step.outcome !== 'skip')
    const total = run.reduce((sum, step) => sum + step.seconds, 0)
    lines.push('| Step | Outcome | Seconds |', '| --- | --- | ---: |',
      ...steps.map(step => `| ${cell(step.step)} | ${cell(step.outcome)} | ${step.outcome === 'skip' ? '' : step.seconds.toFixed(1)} |`),
      `| total of steps run | | ${total.toFixed(1)} |`, '')
  }
  if (tests !== undefined) {
    const slowest = [...tests].sort((a, b) => b.seconds - a.seconds || a.name.localeCompare(b.name)).slice(0, SLOWEST)
    const busy = tests.reduce((sum, test) => sum + test.seconds, 0)
    lines.push(`<details><summary>${Math.min(SLOWEST, tests.length)} slowest of ${tests.length} nextest tests (${busy.toFixed(0)} test-seconds)</summary>`, '',
      '| Test | Binary | Seconds |', '| --- | --- | ---: |',
      ...slowest.map(test => `| ${cell(test.name)}${test.failed ? ' (failed)' : ''} | ${cell(test.suite)} | ${test.seconds.toFixed(2)} |`),
      '', '</details>', '')
  }
  return `${lines.join('\n')}\n`
}

function readIf<T>(path: string, parse: (text: string) => T): T | undefined {
  if (!existsSync(path)) return undefined
  try {
    return parse(readFileSync(path, 'utf8'))
  } catch (error) {
    console.error(`ci-step-summary: cannot read ${path}: ${String(error)}`)
    return undefined
  }
}

if (import.meta.main) {
  const args = process.argv.slice(2)
  const title = args[0] === '--title' && args[1] !== undefined ? args[1] : 'preflight'
  const markdown = renderStepSummary(title, readIf(SUMMARY, parsePreflightSummary), readIf(JUNIT, parseJunit))
  const target = process.env['GITHUB_STEP_SUMMARY']
  if (target === undefined || target === '') process.stdout.write(markdown)
  else appendFileSync(target, markdown)
}
