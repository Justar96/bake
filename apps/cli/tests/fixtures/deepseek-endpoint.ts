/**
 * Point the shipped `deepseek-official` route at a local endpoint the way a
 * user would: a `$DSH_HOME/settings.yaml` `llm-pi-ai` section deep-merges over
 * the base bundle's route per field, while a `--patch` row would replace the
 * adapter's whole config and erase the route's profile.
 * @param baseURL - endpoint the route posts `/v1/messages` under.
 * @returns the settings section, ready to append to a settings document.
 */
export function deepseekEndpointSettings(baseURL: string): string {
  return [
    'llm-pi-ai:',
    '  providers:',
    '    deepseek-official:',
    `      baseURL: ${baseURL}`,
    '',
  ].join('\n')
}
