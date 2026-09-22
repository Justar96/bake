/** Check local Markdown links with the repository's Markdown parser and anchor rules. */
import { execFileSync } from 'node:child_process'
import { resolve } from 'node:path'
import { anchorCache, findViolations } from '../../../scripts/verify-md-links.ts'

const root = resolve(import.meta.dirname, '../../..')
const entryDocs = [
  'README.md', 'README.zh.md', 'CONTRIBUTING.md', 'CONTRIBUTING.zh.md', 'AGENTS.md',
  'docs/architecture.md', 'docs/architecture.zh.md', 'apps/cli/README.md', 'apps/cli/README.zh.md',
  'packages/boot/app-boot/README.md', 'packages/boot/app-boot/README.zh.md',
  'packages/boot/plugin-manager/README.md', 'packages/boot/plugin-manager/README.zh.md',
  'native/system/README.md', 'native/system/README.zh.md',
]
const files = [...entryDocs, ...execFileSync('rg', ['--files', 'apps/tui', '-g', '*.md'], { cwd: root, encoding: 'utf8' })
  .trim().split('\n')].map(file => resolve(root, file))
const anchors = anchorCache()
const errors = files.flatMap(file => findViolations(file, anchors, root))
if (errors.length > 0) {
  for (const error of errors) console.error(`${error.file}:${error.line}: ${error.url} (${error.reason})`)
  process.exitCode = 1
} else console.log(`Bake Markdown links: ${files.length} files passed`)
