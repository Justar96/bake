/**
 * Git runs hooks with repository-local variables such as `GIT_DIR` set; a
 * push from a linked worktree always exports it. A child process that runs
 * `git` against a fixture directory would then act on the repository that ran
 * the hook instead, so the preflight runner and the tests that build fixture
 * repositories drop these variables first.
 * @module scripts/git-env
 */

/** Names `git rev-parse --local-env-vars` prints, which a child must not inherit. */
export const REPOSITORY_GIT_ENV: readonly string[] = [
  'GIT_ALTERNATE_OBJECT_DIRECTORIES',
  'GIT_CONFIG',
  'GIT_CONFIG_PARAMETERS',
  'GIT_CONFIG_COUNT',
  'GIT_OBJECT_DIRECTORY',
  'GIT_DIR',
  'GIT_WORK_TREE',
  'GIT_IMPLICIT_WORK_TREE',
  'GIT_GRAFT_FILE',
  'GIT_INDEX_FILE',
  'GIT_NO_REPLACE_OBJECTS',
  'GIT_REPLACE_REF_BASE',
  'GIT_PREFIX',
  'GIT_SHALLOW_FILE',
  'GIT_COMMON_DIR',
]

/** The numbered pairs `GIT_CONFIG_COUNT` introduces. */
const CONFIG_PAIR = /^GIT_CONFIG_(?:KEY|VALUE)_\d+$/u

/**
 * Copy an environment without the variables that bind git to one repository.
 * @param env - the environment to copy.
 * @returns a new environment; user-level settings such as `GIT_EDITOR` stay.
 */
export function withoutRepositoryGitEnv(env: NodeJS.ProcessEnv): NodeJS.ProcessEnv {
  const copy: NodeJS.ProcessEnv = {}
  for (const [key, value] of Object.entries(env)) {
    if (REPOSITORY_GIT_ENV.includes(key) || CONFIG_PAIR.test(key)) continue
    copy[key] = value
  }
  return copy
}

/**
 * Remove the repository-local variables from an environment in place. Node's
 * child_process reads the live `process.env`, but `Bun.spawn` defaults to the
 * environment the process started with, so Bun callers must pass
 * `env: process.env` for the removal to reach their children.
 * @param env - usually `process.env`.
 */
export function dropRepositoryGitEnv(env: NodeJS.ProcessEnv): void {
  for (const key of Object.keys(env)) if (REPOSITORY_GIT_ENV.includes(key) || CONFIG_PAIR.test(key)) Reflect.deleteProperty(env, key)
}
