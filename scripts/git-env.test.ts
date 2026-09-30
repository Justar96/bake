import { execFileSync } from 'node:child_process'
import { mkdtempSync, readFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { describe, expect, it } from 'bun:test'
import { REPOSITORY_GIT_ENV, withoutRepositoryGitEnv } from './git-env.ts'

describe('withoutRepositoryGitEnv', () => {
  it('covers every repository-local variable this git names', () => {
    const named = execFileSync('git', ['rev-parse', '--local-env-vars'], { encoding: 'utf8' }).split('\n').filter(Boolean)
    expect(REPOSITORY_GIT_ENV).toEqual(expect.arrayContaining(named))
  })

  it('drops repository bindings and numbered config pairs and keeps user settings', () => {
    const env = withoutRepositoryGitEnv({
      PATH: '/bin', GIT_DIR: '/repo/.git/worktrees/x', GIT_INDEX_FILE: '/repo/index', GIT_WORK_TREE: '/repo',
      GIT_CONFIG_COUNT: '1', GIT_CONFIG_KEY_0: 'user.name', GIT_CONFIG_VALUE_0: 'x', GIT_EDITOR: 'vi',
    })
    expect(env).toEqual({ PATH: '/bin', GIT_EDITOR: 'vi' })
  })

  it('keeps a fixture repository separate from the repository a hook runs in', () => {
    const outer = mkdtempSync(join(tmpdir(), 'dsh-git-env-outer-'))
    const fixture = mkdtempSync(join(tmpdir(), 'dsh-git-env-fixture-'))
    try {
      execFileSync('git', ['init', '--quiet', outer], { env: withoutRepositoryGitEnv(process.env) })
      const hookEnv = { ...process.env, GIT_DIR: join(outer, '.git') }
      execFileSync('git', ['init', '--quiet', fixture], { env: withoutRepositoryGitEnv(hookEnv) })
      execFileSync('git', ['-C', fixture, 'config', 'user.name', 'Fixture'], { env: withoutRepositoryGitEnv(hookEnv) })
      expect(readFileSync(join(outer, '.git/config'), 'utf8')).not.toContain('Fixture')
      expect(readFileSync(join(fixture, '.git/config'), 'utf8')).toContain('Fixture')
    } finally {
      rmSync(outer, { recursive: true, force: true })
      rmSync(fixture, { recursive: true, force: true })
    }
  })
})
