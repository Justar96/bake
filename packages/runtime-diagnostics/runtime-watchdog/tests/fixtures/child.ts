/** Boot a Loader composition that mounts the watchdog, then stall the event loop or exhaust the heap. */

import { boot } from 'bake-app-boot'
import * as RuntimeWatchdog from '../../src/index.ts'

const [configPath, scenario] = process.argv.slice(2)
if (configPath === undefined) throw new Error('runtime-watchdog child requires a config path')

const ctx = await boot('runtime-watchdog-test', configPath, undefined, (host) => {
  host.loader.builtins['runtime-watchdog'] = RuntimeWatchdog
})
const sleep = (ms: number) => new Promise(resolve => setTimeout(resolve, ms))

if (scenario === 'stall') {
  await sleep(300)
  const end = performance.now() + 700
  while (performance.now() < end) {
    // Hold the event loop, as a synchronous parse of a large session would.
  }
  await sleep(600)
  await ctx.fiber.dispose()
} else if (scenario === 'oom') {
  const retained: number[][] = []
  for (;;) retained.push(Array.from({ length: 10_000 }, (_, index) => index + retained.length))
} else {
  throw new Error(`unknown scenario ${String(scenario)}`)
}
