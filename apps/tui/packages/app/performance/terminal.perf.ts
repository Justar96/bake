/** Diagnostic whole-process measurements through the built dsh profile and a POSIX PTY. */
import { execFileSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import { existsSync, readFileSync } from 'node:fs'
import { copyFile, mkdtemp, mkdir, writeFile, readFile, rm, realpath } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { parseArgs } from 'node:util'
import { WORKLOADS, summarize, type Sample, type Failure } from './report.ts'
import { Terminal } from './terminal.ts'
import { dictionaries } from '../../ui/src/copy.ts'
import { bundle as build, BUILD_MODE } from '../../../scripts/build.ts'

const ROOT = resolve(import.meta.dirname, '../../../../..')
const APP = join(ROOT, 'apps/tui/packages/app')
const TIMEOUT_MS = 60_000

const { values } = parseArgs({ options: {
  node: { type: 'string', default: 'node' }, samples: { type: 'string', default: '3' },
  workload: { type: 'string', multiple: true }, output: { type: 'string' }, 'cpu-profile': { type: 'string' },
  'app-artifacts': { type: 'string' },
  mode: { type: 'string', default: BUILD_MODE },
} })
const samples = Number(values.samples)
if (!Number.isSafeInteger(samples) || samples < 1) throw new Error('--samples must be a positive integer')
const names = values.workload ?? Object.keys(WORKLOADS)
for (const name of names) if (!Object.hasOwn(WORKLOADS, name)) throw new Error(`Unknown workload ${name}`)
if (values.mode !== 'development' && values.mode !== 'production') throw new Error('--mode must be development or production')
if (process.platform !== 'darwin' && process.platform !== 'linux') throw new Error('The terminal diagnostic requires macOS or Linux for signal-based memory sampling')
await mkdir(join(APP, 'lib'), { recursive: true })
const bundle = await mkdtemp(join(APP, 'lib/perf-'))
const results: (Sample | Failure)[] = []
const sha = (path: string): string => createHash('sha256').update(readFileSync(path)).digest('hex')
const aborter = new AbortController()
const interrupt = (): void => { aborter.abort(new Error('Terminal diagnostic interrupted')) }
process.on('SIGINT', interrupt)
process.on('SIGTERM', interrupt)
try {
  const suppliedApplication = values['app-artifacts'] === undefined ? undefined : resolve(values['app-artifacts'])
  await build((suppliedApplication === undefined ? ['src/index.ts', 'src/startup.ts', 'performance/seed.ts'] : ['performance/seed.ts']).map(entry => join(APP, entry)), bundle, values.mode)
  if (suppliedApplication !== undefined) {
    for (const file of ['index.js', 'startup.js']) await copyFile(join(suppliedApplication, file), join(bundle, file))
  }
  await writeFile(join(bundle, 'metrics.mjs'), await readFile(join(APP, 'performance/metrics.mjs')))
  const sourcePatch = await readFile(join(APP, 'cordis.built.patch.yml'), 'utf8')
  if (values['cpu-profile'] !== undefined) await mkdir(values['cpu-profile'], { recursive: true })
  const metadata = { revision: execFileSync('git', ['rev-parse', 'HEAD'], { cwd: ROOT, encoding: 'utf8' }).trim(),
    dirty: execFileSync('git', ['status', '--porcelain'], { cwd: ROOT, encoding: 'utf8' }).trim() !== '',
    bundleSha256: sha(join(bundle, 'index.js')), bundleBytes: Bun.file(join(bundle, 'index.js')).size,
    mode: values.mode, node: execFileSync(values.node!, ['--version'], { encoding: 'utf8' }).trim(),
    artifacts: Object.fromEntries(['index.js', 'startup.js', 'seed.js', 'metrics.mjs'].map(file => [file, sha(join(bundle, file))])),
    compositionSha256: createHash('sha256').update(sourcePatch).digest('hex'),
    applicationSource: suppliedApplication === undefined ? { kind: 'checkout' } : {
      kind: 'supplied-artifacts', directory: suppliedApplication,
      ...existsSync(join(suppliedApplication, 'metadata.json')) ? { origin: JSON.parse(await readFile(join(suppliedApplication, 'metadata.json'), 'utf8')) as unknown } : {},
    },
    ...suppliedApplication === undefined ? { sourceSha256: createHash('sha256').update(execFileSync('git', ['ls-files', '--cached', '--others', '--exclude-standard', 'apps/tui/packages/app/src', 'apps/tui/packages/ui/src'], { cwd: ROOT, encoding: 'utf8' })
      .trim().split('\n').filter(file => existsSync(join(ROOT, file))).sort().map(file => file + ':' + sha(join(ROOT, file))).join('\n') + sha(join(ROOT, 'apps/tui/scripts/build.ts'))).digest('hex') } : {},
    lockSha256: sha(join(ROOT, 'bun.lock')),
    platform: process.platform, arch: process.arch, viewport: { columns: 120, rows: 40 }, terminalDriver: 'Bun.Terminal', bun: Bun.version,
    samples, workloads: names, workloadSpecifications: Object.fromEntries(names.map(name => [name, WORKLOADS[name as keyof typeof WORKLOADS]])),
    heapLimitMiB: 1024, cpuProfile: values['cpu-profile'] !== undefined,
    clock: 'fresh process; warm filesystem cache; no model/network latency', memory: 'main Node process; forced GC at ready and after streaming; RSS includes worker threads' }
  const save = async (): Promise<void> => {
    if (values.output !== undefined) await writeFile(values.output, JSON.stringify({ metadata, interrupted: aborter.signal.aborted, summary: summarize(results), results }, null, 2) + '\n')
  }
  sampleLoop: for (let iteration = 0; iteration < samples; iteration++) for (const name of names) {
    const root = await realpath(await mkdtemp(join(tmpdir(), 'dsh-tui-perf-')))
    let terminal: Terminal | undefined
    let dimensions: Sample['dimensions'] | undefined
    try {
      aborter.signal.throwIfAborted()
      const { turns } = WORKLOADS[name as keyof typeof WORKLOADS]
      execFileSync(values.node!, [join(bundle, 'seed.js'), root, name], { timeout: TIMEOUT_MS, stdio: 'pipe' })
      dimensions = JSON.parse(await readFile(join(root, 'dimensions.json'), 'utf8')) as Sample['dimensions']
      const home = join(root, 'home')
      const profile = join(home, 'profiles/tui')
      await mkdir(profile, { recursive: true })
      await writeFile(join(profile, 'package.json'), JSON.stringify({ name: 'tui-perf-profile', private: true, dsh: { profile: { bundles: ['@deepseek-ai/dsh-base'] } } }))
      const composition = join(root, 'tui.patch.yml')
      await writeFile(composition, sourcePatch.replace('./lib/index.js', JSON.stringify(join(bundle, 'index.js'))).replace('./lib/startup.js', JSON.stringify(join(bundle, 'startup.js'))))
      const patch = join(root, 'profile.json')
      await writeFile(patch, JSON.stringify([
        { id: 'llm-deepseek', disabled: true }, { id: 'llm-pi-ai', disabled: true }, { id: 'session-title-llm', disabled: true },
        { id: 'agent-default-model', config: { provider: 'perf', model: 'synthetic' } },
        { id: 'session-persistence-jsonl', config: { root: join(home, 'sessions'), compression: 'none' } },
        { id: 'tui-runner', config: { ...turns > 0 ? { resume: dimensions.sessionId } : {}, doubleInterruptMs: 2000, locale: 'en' } },
        { insert: [
          { id: 'perf-replay', name: join(ROOT, 'packages/test-support/llm-replay/lib/index.js'), config: { file: join(root, 'replay.jsonl'), overrideFile: join(root, 'reply.json'), paceMs: 10,
            providers: [{ id: 'perf', models: [{ id: 'synthetic', contextWindow: 1_000_000_000 }] }] } },
        ] },
      ]))
      const metricsFile = join(root, 'metrics.json')
      const env: NodeJS.ProcessEnv = { ...process.env, NODE_ENV: values.mode, DSH_HOME: home, DSH_AGENTS_HOME: join(root, 'agents'), DSH_TUI_PERF_METRICS: metricsFile, TERM: 'xterm-256color', NO_COLOR: '1' }
      delete env.CI
      delete env.NODE_OPTIONS
      delete env.DEEPSEEK_API_KEY
      const start = performance.now()
      terminal = new Terminal([values.node!, ...values['cpu-profile'] === undefined ? [] : ['--cpu-prof', '--cpu-prof-dir=' + resolve(values['cpu-profile']), '--cpu-prof-name=' + name + '-' + iteration + '.cpuprofile'], '--expose-gc', '--max-old-space-size=1024', '--import', join(bundle, 'metrics.mjs'), join(ROOT, 'apps/cli/lib/bin.js'), '--profile', 'tui', '--patch', composition, '--patch', patch], join(root, 'workspace'), metricsFile, env, aborter.signal)
      await terminal.wait('Ink input registration', () => terminal!.inputReady)
      const initialInputMs = await terminal.input('PERF_READY', 'PERF_READY')
      const firstInputMs = performance.now() - start
      const historyMarkersAtFirstInput = terminal.markers.size
      const markerCount = dimensions.historyMarkerCount
      await terminal.wait('complete historical output', () => terminal!.markers.size >= markerCount)
      const readyMs = performance.now() - start
      if (terminal.markers.size !== markerCount || terminal.markerOccurrences !== markerCount) throw new Error(`Expected ${markerCount} historical markers once, got ${terminal.markers.size} unique and ${terminal.markerOccurrences} total markers`)
      for (let marker = 1; marker <= markerCount; marker++) {
        if (!terminal.markers.has(`H${String(marker).padStart(5, '0')}_END`)) throw new Error(`Missing historical marker ${marker}`)
      }
      const initialBytes = terminal.bytes
      const readyMemory = await terminal.sample()
      const idleInputMs: number[] = []
      let draft = 'PERF_READY'
      for (let input = 0; input < 5; input++) { const text = `_${input}`; draft += text; idleInputMs.push(await terminal.input(text, draft)) }
      const idleBytes = terminal.bytes - initialBytes
      terminal.send('\x7f'.repeat(draft.length))
      await terminal.input('PERF_CONTINUE', 'PERF_CONTINUE')
      terminal.text = ''
      const continueStart = performance.now()
      const beforeStream = terminal.bytes
      terminal.send('\r')
      await terminal.wait('first synthetic model delta', () => terminal!.clean.includes('PERF_STREAM_START'))
      const firstDeltaMs = performance.now() - continueStart
      const liveInputMs = await terminal.input('PERF_LIVE_DRAFT', 'PERF_LIVE_DRAFT')
      await terminal.wait('complete synthetic response', () => terminal!.clean.includes('PERF_STREAM_DONE'))
      // The draft typed mid-stream is still there, so idle is the send hint
      // taking the interrupt hint's place.
      await terminal.wait('idle after durable stream completion', () => {
        const clean = terminal!.clean
        const idle = clean.lastIndexOf(dictionaries.en.send)
        return idle > clean.lastIndexOf('PERF_STREAM_DONE') && idle > clean.lastIndexOf(dictionaries.en.interrupt)
      })
      const streamMs = performance.now() - continueStart
      const streamBytes = terminal.bytes - beforeStream
      const settledMemory = await terminal.sample()
      if (terminal.markerOccurrences !== markerCount) throw new Error(`Historical markers were rendered ${terminal.markerOccurrences} times for ${markerCount} expected markers`)
      const result = { workload: name, iteration, dimensions, initialInputMs, firstInputMs, historyMarkersAtFirstInput, readyMs, idleInputMs, idleBytes, initialBytes, firstDeltaMs, liveInputMs, streamMs, streamBytes,
        historyMarkerOccurrences: terminal.markerOccurrences, readyMemory, settledMemory }
      await terminal.quit()
      results.push(result)
      process.stdout.write(JSON.stringify({ workload: name, iteration, initialInputMs, firstInputMs, readyMs, historyMarkersAtFirstInput,
        inputMs: Math.max(...idleInputMs), liveInputMs, initialBytes, idleBytes, streamBytes,
        retainedHeapMiB: readyMemory.afterGc.heapUsed / 1048576, settledRetainedHeapMiB: settledMemory.afterGc.heapUsed / 1048576,
        peakRssMiB: settledMemory.resources.maxRSS / 1024 }) + '\n')
    } catch (error) {
      const failure = { workload: name, iteration, dimensions, error: error instanceof Error ? error.message : String(error) }
      results.push(failure)
      process.stderr.write(JSON.stringify({ ...failure, error: failure.error.split('\n')[0] }) + '\n')
    } finally {
      try { await terminal?.close() } finally { await rm(root, { recursive: true, force: true }) }
    }
    await save()
    if (aborter.signal.aborted) break sampleLoop
  }
  const report = { metadata, interrupted: aborter.signal.aborted, summary: summarize(results), results }
  if (aborter.signal.aborted || results.some(result => 'error' in result)) process.exitCode = 1
  await save()
  if (values.output === undefined) process.stdout.write(JSON.stringify(report, null, 2) + '\n')
  else process.stdout.write(JSON.stringify(report.summary, null, 2) + '\n')
} finally {
  process.off('SIGINT', interrupt)
  process.off('SIGTERM', interrupt)
  await rm(bundle, { recursive: true, force: true })
}
