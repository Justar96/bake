/** Test-only managed tree: a root and one descendant that both ignore SIGTERM and SIGHUP. */

import { spawn } from 'node:child_process'
import { rename, writeFile } from 'node:fs/promises'

const [statePath] = process.argv.slice(2)
if (statePath === undefined) throw new Error('usage: hangup-tree.mjs <state-path>')

process.on('SIGTERM', () => {})
process.on('SIGHUP', () => {})
const descendant = spawn(process.execPath, [
  '-e',
  'process.on("SIGTERM",()=>{});process.on("SIGHUP",()=>{});setInterval(()=>{},60_000)',
], { stdio: 'ignore' })
if (descendant.pid === undefined) throw new Error('managed descendant did not publish a pid')

const pending = `${statePath}.pending-${process.pid}`
await writeFile(pending, JSON.stringify({ root: process.pid, descendant: descendant.pid }))
await rename(pending, statePath)
setInterval(() => {}, 60_000)
