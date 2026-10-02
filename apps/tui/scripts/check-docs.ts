/** Check local Markdown links with the repository's Markdown parser and anchor rules. */
import { execFileSync } from 'node:child_process'
import { existsSync } from 'node:fs'
import { resolve } from 'node:path'
import { anchorCache, findViolations } from '../../../scripts/verify-md-links.ts'

const root = resolve(import.meta.dirname, '../../..')
const entryDocs = [
  'README.md', 'CONTRIBUTING.md', 'AGENTS.md', 'docs/architecture.md', 'apps/cli/README.md',
  'packages/boot/app-boot/README.md', 'packages/boot/plugin-manager/README.md', 'native/system/README.md',
]
// Git, not ripgrep: every checkout has it, and it applies the same ignore rules.
const files = [...entryDocs, ...execFileSync('git', ['ls-files', '--cached', '--others', '--exclude-standard', '--', 'apps/tui/*.md'],
  { cwd: root, encoding: 'utf8' }).trim().split('\n')].map(file => resolve(root, file)).filter(file => existsSync(file))
const anchors = anchorCache()
const errors = files.flatMap(file => findViolations(file, anchors, root))
if (errors.length > 0) {
  for (const error of errors) console.error(`${error.file}:${error.line}: ${error.url} (${error.reason})`)
  process.exitCode = 1
} else console.log(`Bake Markdown links: ${files.length} files passed`)
