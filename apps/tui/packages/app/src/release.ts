/**
 * The running Bake release. Its version and its changelog entry.
 *
 * Both are read from the repository root, which sits the same five hops above
 * this module's directory in the source tree (`packages/app/src`) and in the build output
 * (`packages/app/lib`), so the source launch and the built profile agree.
 *
 * @module @dsh-tui/app/release
 */

import { readFileSync } from 'node:fs'
import { readFile } from 'node:fs/promises'
import { fileURLToPath } from 'node:url'

/** The workspace root that owns `package.json` and `CHANGELOG.md`. */
const ROOT = new URL('../../../../../', import.meta.url)

/**
 * The running release's root directory: a source checkout, or one version
 * directory of a managed install.
 * @returns its absolute path.
 */
export function releaseRoot(): string {
  return fileURLToPath(ROOT)
}

/** Shown when the manifest cannot be read, so the welcome block still draws. */
const UNKNOWN_VERSION = '0.0.0'

/**
 * Read the Bake version from the root manifest.
 * @param root - directory holding `package.json`; defaults to the repository root.
 * @returns the manifest version, or `0.0.0` when it is missing or malformed.
 */
export function bakeVersion(root: URL = ROOT): string {
  try {
    const manifest = JSON.parse(readFileSync(fileURLToPath(new URL('package.json', root)), 'utf8')) as { version?: unknown }
    return typeof manifest.version === 'string' && manifest.version !== '' ? manifest.version : UNKNOWN_VERSION
  } catch {
    return UNKNOWN_VERSION
  }
}

/**
 * Extract one version's section from Keep a Changelog Markdown.
 *
 * A section opens at a level-two heading naming the version, with or without
 * brackets or a leading `v`, and closes at the next level-two heading.
 *
 * @param markdown - the whole changelog.
 * @param version - version to find, without a leading `v`.
 * @returns the section's heading and body, trimmed, or undefined when absent.
 */
export function changelogSection(markdown: string, version: string): string | undefined {
  const lines = markdown.split(/\r?\n/)
  const escaped = version.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
  const heading = new RegExp(`^##\\s+\\[?v?${escaped}\\]?(?:\\s|$)`)
  const start = lines.findIndex(line => heading.test(line))
  if (start < 0) return undefined
  const end = lines.findIndex((line, index) => index > start && /^##\s/.test(line))
  const section = lines.slice(start, end < 0 ? undefined : end).join('\n').trim()
  return section === '' ? undefined : section
}

/**
 * Read the changelog entry for a version.
 * @param version - version to find.
 * @param signal - command cancellation.
 * @param root - directory holding `CHANGELOG.md`; defaults to the repository root.
 * @returns the section, or undefined when the file or the entry is missing.
 */
export async function changelogFor(version: string, signal: AbortSignal, root: URL = ROOT): Promise<string | undefined> {
  let markdown: string
  try {
    markdown = await readFile(fileURLToPath(new URL('CHANGELOG.md', root)), { encoding: 'utf8', signal })
  } catch (error) {
    signal.throwIfAborted()
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') return undefined
    throw error
  }
  return changelogSection(markdown, version)
}
