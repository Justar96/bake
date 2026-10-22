/**
 * Transport factory: creates the appropriate MCP transport based on the
 * plugin's resolved config. Stdio spawns a child process (with credential
 * scrubbing); Streamable HTTP connects to a URL.
 *
 * @module
 */

import { StdioTransport, StreamableHttpTransport, type McpTransport } from '@earendil-works/pi-mcp'
import { scrubbedParentEnv } from 'bake-subprocess'
import type { Config } from './index.ts'

/**
 * The subprocess seam's scrubbed parent env (credential-shaped and stale
 * `DSH_*` names dropped), plus the spec's explicit env. pi-mcp owns the actual
 * spawn, so this transport shares the scrub definition rather than the spawn
 * path.
 */
function buildChildEnv(extra: Record<string, string>): Record<string, string> {
  return { ...scrubbedParentEnv(), ...extra }
}

/**
 * Create an MCP transport from the resolved plugin config.
 *
 * @param config - Resolved plugin config discriminated on `transport`.
 * @returns An unstarted MCP transport (stdio or Streamable HTTP); `McpClient.connect` starts it.
 */
export function createTransport(config: Config): McpTransport {
  switch (config.transport) {
    case 'stdio':
      return new StdioTransport({
        command: config.command,
        args: config.args,
        env: buildChildEnv(config.env),
        // pi-mcp merges the full parent env unless told otherwise; the
        // scrubbed copy above is the complete child environment.
        inheritEnv: false,
        cwd: config.cwd,
        stderr: 'inherit',
      })
    case 'streamable-http':
      return new StreamableHttpTransport({ url: config.url, headers: config.headers })
  }
}
