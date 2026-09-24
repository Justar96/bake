/** Let Windows PowerShell discover its own modules instead of inheriting PowerShell Core's. */
export function windowsPowerShellEnvironment(env: NodeJS.ProcessEnv): NodeJS.ProcessEnv {
  return Object.fromEntries(Object.entries(env).filter(([name]) => name.toUpperCase() !== 'PSMODULEPATH'))
}
