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
  // The minifier writes escaped glyphs back as characters. Windows PowerShell
  // reads a script without a BOM in the ANSI code page, so the payload stays
  // ASCII: every other character, all in the BMP here, returns to an escape.
  return (await output.text()).trim().replace(/[^\x00-\x7f]/gu, (character) => {
    const code = character.codePointAt(0) ?? 0
    if (code > 0xffff) throw new Error(`installer animation: ${character} is outside the BMP`)
    return `\\u${code.toString(16).padStart(4, '0')}`
  })
}

if (import.meta.main) {
  const code = await installerAnimation()
  for (const name of ['install.sh', 'install.ps1']) {
    const path = resolve(import.meta.dir, '../../distribution/host', name)
    const source = readFileSync(path, 'utf8')
    const next = source.replace(/(?<=\/\/ # BEGIN ANIMATION\n)[\s\S]*?(?=\/\/ # END ANIMATION)/, code + '\n')
    if (next === source && !source.includes(code)) throw new Error(`Missing animation markers in ${name}`)
    if (process.argv.includes('--check')) {
      if (next !== source) throw new Error(`Run bun scripts/release/embed-animation.ts to refresh ${name}`)
    } else writeFileSync(path, next)
  }
}
