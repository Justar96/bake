/** Keep installers self-contained: embed the local renderer, never fetch executable progress code. */
import { readFileSync, writeFileSync } from 'node:fs'
import { resolve } from 'node:path'

/** Return the standalone Node program used by both installers. */
export async function installerAnimation(): Promise<string> {
  const result = await Bun.build({
    entrypoints: [resolve(import.meta.dir, 'installer-animation.ts')], target: 'node', format: 'cjs',
    minify: true,
  })
  if (!result.success) throw new AggregateError(result.logs, 'Could not bundle installer animation')
  const output = result.outputs[0]
  if (!output) throw new Error('Missing installer animation bundle')
  return (await output.text()).trim()
}

if (import.meta.main) {
  const code = await installerAnimation()
  for (const name of ['install.sh', 'install.ps1']) {
    const path = resolve(import.meta.dir, '../../distribution/host', name)
    const source = readFileSync(path, 'utf8')
    const next = source.replace(/(?<=\/\/ # BEGIN BAKERY\n)[\s\S]*?(?=\/\/ # END BAKERY)/, code + '\n')
    if (next === source && !source.includes(code)) throw new Error(`Missing bakery markers in ${name}`)
    if (process.argv.includes('--check')) {
      if (next !== source) throw new Error(`Run bun scripts/release/embed-animation.ts to refresh ${name}`)
    } else writeFileSync(path, next)
  }
}
