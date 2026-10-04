/** A release's launch check runs its own command, bounded, in a scratch home it removes, and says why it failed. */
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { basename, dirname, join } from 'node:path'
import { afterEach, describe, expect, it } from 'vitest'
import { failureLine, LAST_RELEASE_WITHOUT_SELF_CHECK, launchProblem, RELEASE_COMMAND } from '../src/index.ts'
import { releaseTree, Scratch, selfCheckingCommand } from './fixture.ts'

const scratches: Scratch[] = []
afterEach(() => { for (const scratch of scratches.splice(0)) scratch.dispose() })

function scratch(): Scratch {
  const owned = new Scratch()
  scratches.push(owned)
  return owned
}

/** A command that records how it was started in `report`, then runs `then`. */
function recording(report: string, then: string): string {
  return [
    'import { writeFileSync } from \'node:fs\'',
    'import { tmpdir } from \'node:os\'',
    `writeFileSync(${JSON.stringify(report)}, JSON.stringify({ argv: process.argv.slice(2), execArgv: process.execArgv, home: process.env.DSH_HOME, tmp: tmpdir() }))`,
    then,
    '',
  ].join('\n')
}

const check = (release: string, version: string, extra: { timeoutMs?: number; signal?: AbortSignal } = {}) =>
  launchProblem({ node: process.execPath, release, version, ...extra })

describe.skipIf(process.platform === 'win32')('launchProblem', () => {
  it('runs a newer release\'s --self-check as a launcher starts it, in a home and temporary directory it then removes', async () => {
    const { root } = scratch()
    const report = join(root, 'report.json')
    const release = releaseTree(join(root, 'release'), recording(report, 'console.log(\'Bake 0.4.0 self-check passed\')'))
    await expect(check(release, '0.4.0')).resolves.toBeUndefined()
    const seen = JSON.parse(readFileSync(report, 'utf8')) as { argv: string[]; execArgv: string[]; home: string; tmp: string }
    expect(seen.argv).toEqual(['--self-check'])
    // The launcher runs directly, without restarting itself for these flags.
    expect(seen.execArgv).toEqual(['--report-exclude-env', '--report-exclude-network', `--diagnostic-dir=${join(seen.home, 'diagnostics')}`])
    // Home and temporary directory share one scratch directory, so removing it removes both.
    expect(dirname(seen.tmp)).toBe(dirname(seen.home))
    expect(basename(dirname(seen.home))).toMatch(/^bake-launch-check-/u)
    expect(existsSync(dirname(seen.home))).toBe(false)
    expect(existsSync(seen.tmp)).toBe(false)
  })

  it(`checks a release no newer than ${LAST_RELEASE_WITHOUT_SELF_CHECK}, which has no --self-check, by its --version`, async () => {
    const { root } = scratch()
    const old = releaseTree(join(root, 'old'), `console.log(process.argv[2] === '--version' ? '${LAST_RELEASE_WITHOUT_SELF_CHECK}' : 'unexpected')\n`)
    await expect(check(old, LAST_RELEASE_WITHOUT_SELF_CHECK)).resolves.toBeUndefined()
    // A newer release without the check fails closed, with the launcher's own complaint.
    const newer = releaseTree(join(root, 'newer'), 'console.error(\'error: --profile <name> is required\'); process.exitCode = 1\n')
    await expect(check(newer, '0.4.0')).resolves.toBe('error: --profile <name> is required')
    const checking = releaseTree(join(root, 'checking'), selfCheckingCommand('0.4.0'))
    await expect(check(checking, '0.4.0')).resolves.toBeUndefined()
  })

  it('fails a release whose layout lacks a package its command imports, quoting the error', async () => {
    const { root } = scratch()
    const release = releaseTree(join(root, 'release'), 'import \'bake-fixture-dependency\'\nconsole.log(\'Bake 0.4.0 self-check passed\')\n')
    const dependency = join(release, 'node_modules/bake-fixture-dependency')
    mkdirSync(dependency, { recursive: true })
    writeFileSync(join(dependency, 'package.json'), JSON.stringify({ name: 'bake-fixture-dependency', type: 'module', exports: './index.js' }))
    writeFileSync(join(dependency, 'index.js'), 'export {}\n')
    await expect(check(release, '0.4.0')).resolves.toBeUndefined()
    rmSync(dependency, { recursive: true })
    expect(await check(release, '0.4.0'))
      .toMatch(/^Error \[ERR_MODULE_NOT_FOUND\]: Cannot find package 'bake-fixture-dependency' imported from /u)
  })

  it('reports the first problem the check names, past Node\'s warnings', async () => {
    const { root } = scratch()
    const release = releaseTree(join(root, 'release'), [
      'console.error(\'(node:123) ExperimentalWarning: something\')',
      'console.error(\'(Use `node --trace-warnings ...` to show where the warning was created)\')',
      'console.error(\'dsh: self-check: tui profile: @scope/plugin: Cannot find package \\\'@scope/plugin\\\'\')',
      'console.error(\'dsh: self-check: agent presets: @scope/other: boom\')',
      'process.exitCode = 1',
      '',
    ].join('\n'))
    await expect(check(release, '0.4.0')).resolves.toBe('dsh: self-check: tui profile: @scope/plugin: Cannot find package \'@scope/plugin\'')
  })

  it('fails a command that is missing, or that reports another version', async () => {
    const { root } = scratch()
    await expect(check(join(root, 'empty'), '0.4.0')).resolves.toBe(`${RELEASE_COMMAND} is missing`)
    const release = releaseTree(join(root, 'release'), 'console.log(\'Bake 0.4.1 self-check passed\')\n')
    await expect(check(release, '0.4.0')).resolves.toBe('it reported Bake 0.4.1 self-check passed, not 0.4.0')
  })

  it('stops a check that outlives its bound, and removes what it wrote', async () => {
    const { root } = scratch()
    const report = join(root, 'report.json')
    const release = releaseTree(join(root, 'release'), recording(report, [
      'import { mkdirSync } from \'node:fs\'',
      'mkdirSync(tmpdir() + \'/bake-self-check-private\', { recursive: true })',
      'setInterval(() => {}, 1000)',
    ].join('\n')))
    const started = Date.now()
    await expect(check(release, '0.4.0', { timeoutMs: 300 })).resolves.toBe('no result within 300 ms')
    expect(Date.now() - started).toBeLessThan(5000)
    const seen = JSON.parse(readFileSync(report, 'utf8')) as { home: string; tmp: string }
    expect(existsSync(seen.home)).toBe(false)
    expect(existsSync(seen.tmp)).toBe(false)
  })

  it('stops the command and throws when cancelled', async () => {
    const { root } = scratch()
    const report = join(root, 'report.json')
    const release = releaseTree(join(root, 'release'), recording(report, 'setInterval(() => {}, 1000)'))
    const abort = new AbortController()
    const running = check(release, '0.4.0', { signal: abort.signal })
    while (!existsSync(report)) await new Promise(resolve => setTimeout(resolve, 10))
    abort.abort()
    await expect(running).rejects.toMatchObject({ name: 'AbortError' })
    expect(existsSync((JSON.parse(readFileSync(report, 'utf8')) as { home: string }).home)).toBe(false)
  })
})

describe('failureLine', () => {
  it('prefers an uncaught error\'s message to the source line Node prints above it', () => {
    const stderr = [
      'node:internal/modules/esm/resolve:873',
      '  throw new ERR_MODULE_NOT_FOUND(packageName, fileURLToPath(base), null);',
      '        ^',
      '',
      'Error [ERR_MODULE_NOT_FOUND]: Cannot find package \'commander\' imported from /r/apps/cli/lib/cli.js',
      '    at packageResolve (node:internal/modules/esm/resolve:873:9)',
    ].join('\n')
    expect(failureLine(stderr, '')).toBe('Error [ERR_MODULE_NOT_FOUND]: Cannot find package \'commander\' imported from /r/apps/cli/lib/cli.js')
    expect(failureLine('file:///r/bin.js:3\nlet let\n    ^^^\n\nSyntaxError: Unexpected strict mode reserved word\n', '')).toBe('SyntaxError: Unexpected strict mode reserved word')
  })

  it('falls back to the first line of either stream, shortened, and to nothing', () => {
    expect(failureLine('', 'only stdout\nsecond')).toBe('only stdout')
    expect(failureLine('x'.repeat(500))).toHaveLength(400)
    expect(failureLine('\n  \n', '')).toBeUndefined()
  })
})
