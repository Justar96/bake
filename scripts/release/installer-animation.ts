/** Embedded in each installer: only local status text crosses this boundary. */
import { readFileSync } from 'node:fs'
import { startBakery } from '../../apps/cli/src/bakery.ts'

const statusFile = process.argv[2]
if (!statusFile) throw new Error('Missing installer progress file')
const parent = Number(process.argv[3])
const bakery = startBakery(process.stderr, process.env, 'Warming the oven...')
const timer = setInterval(() => {
  try {
    process.kill(parent, 0)
    const stage = readFileSync(statusFile, 'utf8').trim()
    if (stage === 'done' || stage === 'stop') {
      clearInterval(timer)
      bakery.finish(stage === 'done')
    } else if (stage) bakery.stage(stage)
  } catch {
    clearInterval(timer)
    bakery.finish()
  }
}, 100)
for (const signal of ['SIGINT', 'SIGTERM'] as const) {
  process.once(signal, () => { clearInterval(timer); bakery.finish() })
}
