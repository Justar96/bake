/** Approve pnpm's pending dependency scripts in the current profile's workspace settings. */
import { readFile } from 'node:fs/promises'
import { join } from 'node:path'
import { isAlias, isMap, isNode, isScalar, isSeq, parseDocument, visit } from 'yaml'
import { ManagementFailure } from './failure.ts'
import { writeFileAtomic } from '@deepseek-ai/dsh-atomic-write'

async function readIgnoredBuilds(dir: string): Promise<string[]> {
  let text: string
  try { text = await readFile(join(dir, 'node_modules', '.modules.yaml'), 'utf8') }
  catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') return []
    throw error
  }
  const document = parseDocument(text)
  if (document.errors[0] !== undefined) throw document.errors[0]
  const ignored = document.get('ignoredBuilds', true)
  if (ignored === undefined) return []
  if (!isSeq(ignored)) {
    throw new Error('pnpm ignoredBuilds must be a list of package selectors')
  }
  const selectors = ignored.items.flatMap(item => isScalar(item) && typeof item.value === 'string' ? [item.value] : [])
  if (selectors.length !== ignored.items.length) throw new Error('pnpm ignoredBuilds must be a list of package selectors')
  return selectors
}

async function readPolicy(dir: string) {
  let text: string
  try { text = await readFile(join(dir, 'pnpm-workspace.yaml'), 'utf8') }
  catch (error) {
    if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error
    text = '{}\n'
  }
  const document = parseDocument(text)
  if (document.errors[0] !== undefined) throw document.errors[0]
  if (!isMap(document.contents)) throw new Error('pnpm-workspace.yaml must be a YAML mapping')
  const builds = document.get('allowBuilds')
  if (builds !== undefined && !isMap(builds)) throw new Error('allowBuilds must be a YAML mapping')
  visit(builds ?? null, (_key, node) => {
    if (isAlias(node) || (isNode(node) && 'anchor' in node && node.anchor)) {
      throw new Error('allowBuilds must not contain YAML anchors or aliases')
    }
  })
  const placeholders = isMap(builds) ? builds.items.flatMap(({ key, value }) =>
    isScalar(key) && typeof key.value === 'string' && !/[*?]/.test(key.value)
      && isScalar(value) && value.value === 'set this to true or false' ? [key.value] : []) : []
  const decisions = isMap(builds) ? builds.items.flatMap(({ key, value }) =>
    isScalar(key) && typeof key.value === 'string' && isScalar(value) && typeof value.value === 'boolean'
      ? [key.value] : []) : []
  const decided = (selector: string): boolean => {
    const at = selector.lastIndexOf('@')
    const packageName = at > 0 ? selector.slice(0, at) : selector
    return decisions.some((rule) => {
      const pattern = rule.replace(/[.+^${}()|[\]\\]/g, '\\$&').replace(/\*/g, '.*').replace(/\?/g, '.')
      return new RegExp(`^${pattern}$`).test(selector) || new RegExp(`^${pattern}$`).test(packageName)
    })
  }
  // Newer pnpm 11 records ignored scripts in its profile metadata instead of
  // writing an undecided allowBuilds entry to the workspace file.
  const ignored = (await readIgnoredBuilds(dir)).filter(name => !/[*?]/.test(name) && !decided(name))
  const pending = [...new Set([...placeholders, ...ignored])]
  return { document, pending }
}

/** Read undecided package selectors from pnpm 11's workspace settings and ignored-build metadata.
 * @param dir Current profile directory.
 * @returns Exact package selectors awaiting a build decision; wildcard rules are excluded.
 */
export async function readPendingBuilds(dir: string): Promise<string[]> {
  return (await readPolicy(dir)).pending
}

/** Persist approval without running scripts; the caller holds the profile manifest lock.
 * @param dir Current profile directory.
 * @param names Explicit package selectors from the pending build list.
 * @throws If a selector is no longer pending or allowBuilds contains YAML anchors or aliases; no approvals are written.
 */
export async function approveBuilds(dir: string, names: readonly string[]): Promise<void> {
  const { document, pending } = await readPolicy(dir)
  if (names.some(name => !pending.includes(name))) throw new ManagementFailure('stale-approval')
  if (names.length === 0) return
  for (const name of names) document.setIn(['allowBuilds', name], true)
  await writeFileAtomic(join(dir, 'pnpm-workspace.yaml'), String(document), { mode: 0o600 })
}
