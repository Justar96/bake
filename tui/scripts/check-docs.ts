/** Check local Markdown links with the repository's Markdown parser and anchor rules. */
import { execFileSync } from 'node:child_process'
import { resolve } from 'node:path'
import { anchorCache, findViolations } from '../../scripts/verify-md-links.ts'

const root = resolve(import.meta.dirname, '../..')
const files = execFileSync('rg', ['--files', 'tui', '-g', '*.md'], { cwd: root, encoding: 'utf8' })
  .trim().split('\n').map(file => resolve(root, file))
const anchors = anchorCache()
const errors = files.flatMap(file => findViolations(file, anchors, root))
if (errors.length > 0) {
  for (const error of errors) console.error(`${error.file}:${error.line}: ${error.url} (${error.reason})`)
  process.exitCode = 1
} else console.log(`TUI Markdown links: ${files.length} files passed`)
