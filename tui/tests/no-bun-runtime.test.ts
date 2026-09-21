/**
 * The shipped plugin runs inside the dsh process, which is Node and cannot be
 * Bun: `app-boot` reaches V8 current-context symbols during host preparation,
 * and JavaScriptCore has none (tui/PLAN.md §2.2). A `bun:` import or a `Bun.*`
 * global in `packages/` would therefore make the plugin unloadable — at boot,
 * in the user's terminal, not here.
 *
 * Bun stays in the harness, the fixtures, and this test file itself.
 */

import { describe, expect, it } from 'bun:test'
import { readdirSync, readFileSync, statSync } from 'node:fs'
import { join } from 'node:path'

/** Source roots that end up inside the dsh process. */
const SHIPPED = [
  new URL('../packages/app/src', import.meta.url).pathname,
  new URL('../packages/ui/src', import.meta.url).pathname,
]

/** Bun surfaces that cannot resolve under Node. */
const FORBIDDEN = [
  { pattern: /from\s+['"]bun:[^'"]+['"]/, label: 'a bun: module import' },
  { pattern: /require\(\s*['"]bun:[^'"]+['"]\s*\)/, label: 'a bun: require' },
  { pattern: /\bBun\s*\./, label: 'the Bun global' },
]

/**
 * List every TypeScript source file under a root, recursively.
 *
 * @param root - directory to walk.
 * @returns absolute paths of the `.ts` and `.tsx` files found.
 */
function sources(root: string): string[] {
  const found: string[] = []
  for (const entry of readdirSync(root)) {
    const path = join(root, entry)
    if (statSync(path).isDirectory()) found.push(...sources(path))
    else if (path.endsWith('.ts') || path.endsWith('.tsx')) found.push(path)
  }
  return found
}

describe('shipped plugin code', () => {
  it('imports nothing that only exists under Bun', () => {
    const offenses: string[] = []
    for (const root of SHIPPED) {
      for (const file of sources(root)) {
        const text = readFileSync(file, 'utf8')
        for (const { pattern, label } of FORBIDDEN) {
          if (pattern.test(text)) offenses.push(`${file}: ${label}`)
        }
      }
    }
    expect(offenses).toEqual([])
  })

  it('scans a non-empty set of files', () => {
    // A walk that silently finds nothing would pass the check above forever.
    expect(SHIPPED.flatMap(sources).length).toBeGreaterThan(0)
  })
})
