/** Shiki highlighting as the presentation layer calls it. Synchronous, and plain until a grammar is ready. */
import { afterEach, describe, expect, it, vi } from 'vitest'
import { createSyntax, languageOf, type Syntax } from '../src/syntax.ts'

describe('syntax', () => {
  let syntax: Syntax | undefined
  afterEach(async () => {
    await syntax?.close()
    syntax = undefined
  })

  it('names a language by extension or file name, and none for an unknown one', () => {
    expect(languageOf('packages/app/src/runner.ts')).toBe('typescript')
    expect(languageOf('ui/app.tsx')).toBe('tsx')
    expect(languageOf('scripts/build.mjs')).toBe('javascript')
    expect(languageOf('Cargo.toml')).toBe('toml')
    expect(languageOf('docker/Dockerfile')).toBe('docker')
    expect(languageOf('main.rb')).toBe('ruby')
    expect(languageOf('notes.unknown-extension')).toBeUndefined()
    expect(languageOf('events.jsonl')).toBe('json')
    expect(languageOf('events.ndjson')).toBe('json')
    expect(languageOf('C:\\project\\Dockerfile')).toBe('docker')
    expect(languageOf('LICENSE')).toBeUndefined()
  })

  it('colours a preloaded language before the first frame, covering each line', async () => {
    syntax = createSyntax()
    await syntax.ready
    const lines = ['const home = process.env.HOME', '// where']
    const tokens = syntax.highlight(lines, 'a.ts')
    expect(tokens).toHaveLength(2)
    expect(tokens!.map(line => line.reduce((sum, token) => sum + token.length, 0))).toEqual(lines.map(line => line.length))
    // Accents are the theme's; its base tones are left to the terminal, and
    // a comment is drawn dim.
    expect(tokens![0]!.some(token => token.color !== undefined)).toBe(true)
    expect(tokens!.flat().every(token => token.color === undefined || token.color !== '#839496')).toBe(true)
    expect(tokens![1]!.at(-1)).toMatchObject({ dim: true })
  })

  it('leaves another language plain until its grammar loads, then colours it', async () => {
    syntax = createSyntax()
    await syntax.ready
    expect(syntax.highlight(['def main; end'], 'main.rb')).toBeUndefined()
    const ready = syntax
    await vi.waitFor(() => { expect(ready.highlight(['def main; end'], 'main.rb')).toBeDefined() })
    expect(syntax.highlight(['anything'], 'notes.unknown-extension')).toBeUndefined()
  })

  it('draws nothing once closed, and closing awaits grammars still loading', async () => {
    syntax = createSyntax()
    await syntax.ready
    expect(syntax.highlight(['fn main() {}'], 'a.zig')).toBeUndefined()
    await syntax.close()
    expect(syntax.highlight(['const a = 1'], 'a.ts')).toBeUndefined()
    expect(syntax.highlight(['fn main() {}'], 'a.zig')).toBeUndefined()
  })
})
