// Records the model-facing MCP tool names that Bake's TypeScript computes,
// for `bake-coding-agent`'s `mcp::names` tests. `publicToolName` below is
// copied verbatim (types removed) from
// packages/mcp/mcp-client/src/tools.ts; that module's other imports need
// the workspace's packages, which this script does not.
//
//   node public-names.mjs > public-names.json
import { createHash } from 'node:crypto'

const MAX_PUBLIC_NAME_LENGTH = 64
const INVALID_NAME_CHARS = /[^A-Za-z0-9_-]/g
const HASH_LENGTH = 12

function publicToolName(serverName, rawName) {
  const joined = `mcp__${serverName}__${rawName}`
  const normalized = joined.replace(INVALID_NAME_CHARS, '_')
  if (normalized === joined && normalized.length <= MAX_PUBLIC_NAME_LENGTH) return normalized
  const hash = createHash('sha256').update(`${serverName}\0${rawName}`).digest('hex').slice(0, HASH_LENGTH)
  return `${normalized.slice(0, MAX_PUBLIC_NAME_LENGTH - HASH_LENGTH - 1)}_${hash}`
}

const cases = [
  ['github', 'create_issue'],
  ['everything', 'get-sum'],
  ['srv', 'admin.reset'],
  ['srv', 'admin_reset'],
  ['srv', 'a'.repeat(80)],
  ['srv', `${'a'.repeat(80)}b`],
  ['my-server', 'get.item/v2'],
  ['s', 'a😀'],
  ['s', 'é'],
  ['a', 'b__c'],
  ['a__b', 'c'],
  ['x'.repeat(32), 'y'.repeat(27)],
  ['x'.repeat(32), 'y'.repeat(28)],
  ['fixture', 'echo'],
]
process.stdout.write(`${JSON.stringify(cases.map(([s, t]) => [s, t, publicToolName(s, t)]), null, 2)}\n`)
