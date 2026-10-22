/**
 * A stand-in agent for `run.test.ts`: installed as a fake Bake checkout's
 * `apps/cli/lib/bin.js` or as a fake pi CLI, it records how the evaluator
 * launched it and exits 0 without output. It sends no request, touches no
 * fixture file, and so never makes a sample succeed.
 *
 * The evaluator deletes each sample root once the agent exits, so every file
 * the launch configured is copied into the capture while the agent still runs.
 * The capture directory comes from EVAL_CONTRACT_CAPTURE, which the test sets
 * on the evaluator and the evaluator passes through to every agent.
 *
 * With EVAL_CONTRACT_HANG=1 the recorder instead hangs after recording: it
 * ignores SIGTERM, announces itself as `<pid>.hung` in the capture directory,
 * and never exits, so the test can exercise teardown of a stuck agent.
 */
import { existsSync, readFileSync, writeFileSync } from 'node:fs'
import { dirname, join } from 'node:path'

const read = path => typeof path === 'string' && existsSync(path) ? readFileSync(path, 'utf8') : null
const after = flag => { const at = process.argv.indexOf(flag); return at === -1 ? undefined : process.argv[at + 1] }
const env = process.env
const sampleRoot = dirname(process.cwd())

const capture = {
  script: process.argv[1],
  args: process.argv.slice(2),
  cwd: process.cwd(),
  env: Object.fromEntries(['NO_COLOR', 'BAKE_HOME', 'DSH_HOME', 'BAKE_AGENTS_HOME', 'DSH_AGENTS_HOME', 'PI_CODING_AGENT_DIR', 'PI_OFFLINE', 'EVAL_API_KEY']
    .map(name => [name, env[name] ?? null])),
  secretNames: Object.keys(env).filter(name => /KEY|TOKEN|SECRET|PASSWORD/i.test(name)).sort(),
  patch: read(after('--patch')),
  extension: read(after('-e')),
  hook: read(join(sampleRoot, 'stale-writer.mjs')),
  bakeSettings: read(env.BAKE_HOME && join(env.BAKE_HOME, 'settings.yaml')),
  bakeCredentials: read(env.BAKE_HOME && join(env.BAKE_HOME, '.credentials.yaml')),
  piModels: read(env.PI_CODING_AGENT_DIR && join(env.PI_CODING_AGENT_DIR, 'models.json')),
  piSettings: read(env.PI_CODING_AGENT_DIR && join(env.PI_CODING_AGENT_DIR, 'settings.json')),
}
writeFileSync(join(env.EVAL_CONTRACT_CAPTURE, `${Date.now()}-${process.pid}.json`), JSON.stringify(capture), { flag: 'wx' })
if (env.EVAL_CONTRACT_HANG === '1') {
  process.on('SIGTERM', () => {})
  writeFileSync(join(env.EVAL_CONTRACT_CAPTURE, `${process.pid}.hung`), '', { flag: 'wx' })
  setInterval(() => {}, 1 << 30)
}
