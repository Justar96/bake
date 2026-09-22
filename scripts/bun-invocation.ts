/** Resolve shell-free child-process invocations for the Bun process that launched a package script. */

/**
 * Resolve Bun's executable and arguments from its lifecycle environment.
 * @param args - Arguments to pass to Bun.
 * @param environment - Lifecycle environment containing `npm_execpath`.
 * @returns A command and argument array suitable for `spawn` or `spawnSync` without a shell.
 */
export function bunInvocation(
  args: readonly string[],
  environment: NodeJS.ProcessEnv = process.env,
): { command: string; args: string[] } {
  const entrypoint = environment.npm_execpath
  if (entrypoint === undefined || entrypoint === '') {
    throw new Error('bun invocation: npm_execpath is unavailable; invoke the script through bun run.')
  }
  return { command: entrypoint, args: [...args] }
}
