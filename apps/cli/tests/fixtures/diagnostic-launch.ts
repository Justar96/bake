import { execFileSync } from 'node:child_process'
import { restartWithDiagnostics } from '../../src/diagnostic-launch.ts'

if (process.argv.includes('--spawn-fallback')) process.execve = undefined
await restartWithDiagnostics()

const mode = process.argv[2]
if (mode === 'signal') {
  for (const signal of ['SIGINT', 'SIGTERM', 'SIGHUP'] as const) {
    process.on(signal, () => {
      process.stdout.write(`${signal}\n`)
      process.exit(signal === 'SIGINT' ? 130 : signal === 'SIGHUP' ? 129 : 0)
    })
  }
  setInterval(() => {}, 1_000)
  process.stdout.write('ready\n')
} else if (mode === 'signal-death') {
  setInterval(() => {}, 1_000)
  process.stdout.write('ready\n')
} else {
  const descendant = JSON.parse(execFileSync(process.execPath, ['-e', 'console.log(JSON.stringify({execArgv: process.execArgv, nodeOptions: process.env.NODE_OPTIONS}))'], { encoding: 'utf8' }))
  process.stdout.write(JSON.stringify({
    pid: process.pid, parent: process.ppid, execArgv: process.execArgv, argv: process.argv.slice(2),
    home: process.env.DSH_HOME, nodeOptions: process.env.NODE_OPTIONS,
    excludeEnv: process.report.excludeEnv, excludeNetwork: process.report.excludeNetwork, descendant,
  }) + '\n')
  process.exitCode = mode === 'failure' ? 17 : 0
}
