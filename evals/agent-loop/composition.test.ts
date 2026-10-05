/** The eval's composition overlay matches what each checkout ships, under either package naming. */
import { afterAll, describe, expect, test } from 'bun:test'
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import {
  BASE_BUNDLE, compositionCheck, HEADLESS_BUNDLE, layeredConfig, presetPersonaPrefix, rosterOverlay, STANDARD_PRESET,
  systemPromptOf, systemPromptOverlay, TUI_PATCH,
} from './composition.ts'

const ROOT = fileURLToPath(new URL('../..', import.meta.url))
const temporary: string[] = []
afterAll(() => { for (const dir of temporary) rmSync(dir, { recursive: true, force: true }) })

/** A checkout holding only the composition files, as `{ relative path: text }`. */
function checkout(files: Record<string, string>): string {
  const root = mkdtempSync(join(tmpdir(), 'bake-eval-composition-'))
  temporary.push(root)
  for (const [path, text] of Object.entries(files)) {
    mkdirSync(dirname(join(root, path)), { recursive: true })
    writeFileSync(join(root, path), text)
  }
  return root
}

describe('the current tree', () => {
  test('keeps the headless system-prompt config and swaps in the standard persona prefix', () => {
    const row = systemPromptOverlay(ROOT) as { id: string; config: Record<string, unknown> }
    expect(row.id).toBe('system-prompt')
    expect(row.config.includeHarnessIdentity).toBe(false)
    expect(row.config.personaSuffix).toBe('Your working directory is {{cwd}}.')
    expect(row.config.personaPrefix).toBe(presetPersonaPrefix(ROOT))
    expect(String(row.config.personaPrefix)).toStartWith('You are Bake, a coding agent in the user\'s terminal')
    // Every key the headless bundle sets survives; only the prefix differs.
    const bundle = layeredConfig(ROOT, [BASE_BUNDLE, HEADLESS_BUNDLE], 'system-prompt')!
    expect(Object.keys(row.config).sort()).toEqual(Object.keys(bundle).sort())
    expect({ ...row.config, personaPrefix: null }).toEqual({ ...bundle, personaPrefix: null })
  })

  test('the tui roster copies the standard preset tool configs and the terminal inline cap', () => {
    expect(rosterOverlay(ROOT, 'headless')).toEqual([])
    const rows = Object.fromEntries(rosterOverlay(ROOT, 'tui').map(row => [row.id, row.config]))
    expect(rows['tool-fs']).toEqual({ readMaxBytes: 16384 })
    expect(rows['tool-fs-search']).toEqual({ sampleOverCapGlobResults: false })
    expect(rows['tool-result-pruner']).toEqual({ thresholdChars: 8192, headChars: 4096, tailChars: 1024 })
    expect(rows['spill-policy']).toEqual({ maxInlineBytes: 16384 })
    // The headless base caps inline output at 50,000 bytes; the roster is what changes it.
    expect(layeredConfig(ROOT, [BASE_BUNDLE], 'spill-policy')).toEqual({ maxInlineBytes: 50000 })
    expect(Object.keys(rows)).not.toContain('tool-ask-user')
  })
})

/** Excerpts of the composition files as they stood before the bake-* rename (c47efcb020), rows verbatim. */
const PRE_RENAME = {
  [HEADLESS_BUNDLE]: `# The persona owns the identity, so the harness opener stays out.
- id: system-prompt
  config:
    includeHarnessIdentity: false
    personaSuffix: Your working directory is {{cwd}}.
    personaPrefix: |-
      You are Bake, a coding agent running one task from the command line, using the {{model}} model.

      Follow the conventions of the surrounding code.

- id: tools
  config:
    mode: !!js process.env.DSH_TOOLS_MODE

- insert:
    - id: headless-startup
      name: '@deepseek-ai/dsh-headless/startup'
`,
  [BASE_BUNDLE]: `- insert:
    - id: tool-fs
      name: '@deepseek-ai/dsh-tool-fs'

    - id: spill-policy
      name: '@deepseek-ai/dsh-spill-policy'
      config:
        maxInlineBytes: 50000

    - id: system-prompt
      name: '@deepseek-ai/dsh-system-prompt'
      config:
        personaPrefix: ''
`,
  [STANDARD_PRESET]: `- id: persona
  name: '@deepseek-ai/dsh-persona'
  config:
    suffix: Your working directory is {{cwd}}.
    prefix: |-
      You are Bake, a coding agent in the user's terminal, using the {{model}} model. The user sees your tool calls and Markdown replies.

      Follow the conventions of the surrounding code.

- id: tool-fs
  name: '@deepseek-ai/dsh-tool-fs'
  config:
    readMaxBytes: 16384

- id: compaction
  name: cordis:group
  group: true
  config:
    - id: tool-result-pruner
      name: '@deepseek-ai/dsh-compaction-tool-result-pruner'
      config:
        thresholdChars: 8192
        headChars: 4096
        tailChars: 1024
`,
}

describe('a pre-rename checkout', () => {
  test('resolves rows by id under @deepseek-ai/dsh-* names', () => {
    const root = checkout(PRE_RENAME)
    expect(systemPromptOverlay(root)).toEqual({
      id: 'system-prompt',
      config: {
        includeHarnessIdentity: false,
        personaSuffix: 'Your working directory is {{cwd}}.',
        personaPrefix: 'You are Bake, a coding agent in the user\'s terminal, using the {{model}} model. The user sees your tool calls and Markdown replies.\n\nFollow the conventions of the surrounding code.',
      },
    })
  })

  test('falls back to the base inline cap when its terminal patch sets none', () => {
    const root = checkout({ ...PRE_RENAME, [TUI_PATCH]: '- id: tool-jobs\n  disabled: true\n' })
    expect(rosterOverlay(root, 'tui')).toEqual([
      { id: 'tool-fs', config: { readMaxBytes: 16384 } },
      { id: 'tool-result-pruner', config: { thresholdChars: 8192, headChars: 4096, tailChars: 1024 } },
      { id: 'spill-policy', config: { maxInlineBytes: 50000 } },
    ])
  })

  test('refuses a system-prompt config it cannot restate as JSON', () => {
    const root = checkout({ ...PRE_RENAME, [HEADLESS_BUNDLE]: '- id: system-prompt\n  config:\n    personaSuffix: !!js process.cwd()\n' })
    expect(() => systemPromptOverlay(root)).toThrow(/!!js/)
  })
})

describe('the rendered prompt', () => {
  test('is read from each wire format', () => {
    expect(systemPromptOf({ system: [{ type: 'text', text: 'A' }, { type: 'text', text: 'B' }] })).toBe('AB')
    expect(systemPromptOf({ system: 'S', messages: [{ role: 'user', content: 'hi' }] })).toBe('S')
    expect(systemPromptOf({ instructions: 'I', input: [{ role: 'developer', content: [{ type: 'input_text', text: 'D' }] }] })).toBe('I\n\nD')
    expect(systemPromptOf({ messages: [{ role: 'system', content: 'C' }, { role: 'user', content: 'u' }] })).toBe('C')
    expect(systemPromptOf({ messages: [{ role: 'user', content: 'u' }] })).toBeNull()
  })

  test('passes only without the harness opener and with the working-directory line', () => {
    const shipped = 'You are Bake.\n\nYour working directory is /tmp/w.'
    expect(compositionCheck(shipped, ['/tmp/w'])).toEqual({ harnessOpener: false, cwdLine: true, ok: true })
    expect(compositionCheck(`You are an AI agent powered by DeepSeek Harness.\n\n${shipped}`, ['/tmp/w']).ok).toBe(false)
    expect(compositionCheck('You are Bake.', ['/tmp/w'])).toEqual({ harnessOpener: false, cwdLine: false, ok: false })
    expect(compositionCheck('Your working directory is /private/tmp/w.', ['/tmp/w', '/private/tmp/w']).cwdLine).toBe(true)
  })
})
