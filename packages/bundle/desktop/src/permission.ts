/**
 * The desktop's three permission tiers as Bake permission presets plus a
 * per-call classifier. Presets set the sandbox mode and approval policy;
 * the classifier adds what presets cannot express: which tools ask in the
 * `normal` tier and which are refused in `read-only`.
 * @module @deepseek-ai/dsh-desktop/permission
 */

import type { PermissionTier } from './protocol.ts'

/** The Bake permission preset each tier selects for a Session. */
export const TIER_PRESET: Readonly<Record<PermissionTier, string>> = {
  'read-only': 'read-only',
  'normal': 'workspace-write',
  'full-access': 'danger-full-access',
}

/** Tools that only read the workspace, the web, or the agent's own state. */
const READ_TOOLS = new Set([
  'read', 'glob', 'grep', 'read_image', 'web_fetch', 'web_search', 'todo_write',
  'list_agents', 'list_subagent_models', 'skill', 'exit_plan_mode', 'ask_user_question',
])

/** Workspace file writes; the sandbox still asks before a write outside the workspace. */
const WRITE_TOOLS = new Set(['write', 'edit'])

/** Sub-agent orchestration; children run under their parent's sandbox. */
const AGENT_TOOLS = new Set(['subagent', 'subagent_fork', 'send_message', 'interrupt_agent'])

/** Shell tools, whose effect depends on the command. */
const SHELL_TOOLS = new Set(['bash', 'pwsh'])

/** Longest command or argument preview shown on an approval card. */
const SUMMARY_CHARS = 240

/** What the classifier decided for one call. */
export type ToolVerdict =
  | { kind: 'allow' }
  | { kind: 'deny'; reason: string }
  | { kind: 'ask'; summary: string; grant?: { key: string; label: string } }

/** Characters that combine or substitute commands, so one program name no longer describes the call. */
const COMPOUND = /[;&|`\n<>]|\$\(/

function preview(text: string): string {
  const oneLine = text.replace(/\s+/g, ' ').trim()
  return oneLine.length > SUMMARY_CHARS ? `${oneLine.slice(0, SUMMARY_CHARS)}…` : oneLine
}

/**
 * The program a simple shell command runs, skipping leading `NAME=value`
 * assignments. Returns `undefined` for compound commands (pipes, lists,
 * substitutions, redirections), which get no per-program session grant.
 */
export function commandProgram(command: string): string | undefined {
  if (COMPOUND.test(command)) return undefined
  const words = command.trim().split(/\s+/)
  const program = words.find(word => !/^[A-Za-z_][A-Za-z0-9_]*=/.test(word))
  if (program === undefined || program === '') return undefined
  return program
}

function shellVerdict(name: string, args: unknown): ToolVerdict {
  const command = typeof args === 'object' && args !== null && typeof (args as { command?: unknown }).command === 'string'
    ? (args as { command: string }).command
    : undefined
  if (command === undefined) return { kind: 'ask', summary: `${name} (no command)` }
  const program = commandProgram(command)
  return {
    kind: 'ask',
    summary: preview(command),
    ...program === undefined ? {} : { grant: { key: `shell:${program}`, label: `\`${program}\` commands` } },
  }
}

/**
 * Decides one tool call under a tier.
 * - `full-access` allows everything.
 * - `normal` allows read, workspace-write, and sub-agent tools; shell commands
 *   and every other tool (MCP servers, jobs, code execution) ask.
 * - `read-only` allows read and sub-agent tools and shell commands (which the
 *   read-only sandbox confines); writes and every other tool are refused.
 */
export function classifyTool(tier: PermissionTier, name: string, args: unknown): ToolVerdict {
  if (tier === 'full-access') return { kind: 'allow' }
  if (READ_TOOLS.has(name) || AGENT_TOOLS.has(name)) return { kind: 'allow' }
  if (tier === 'read-only') {
    if (SHELL_TOOLS.has(name)) return { kind: 'allow' }
    return { kind: 'deny', reason: `${name} is not available in read-only mode` }
  }
  if (WRITE_TOOLS.has(name)) return { kind: 'allow' }
  if (SHELL_TOOLS.has(name)) return shellVerdict(name, args)
  let rendered: string
  try {
    rendered = args === undefined ? '' : JSON.stringify(args)
  } catch {
    rendered = ''
  }
  return {
    kind: 'ask',
    summary: preview(`${name} ${rendered}`),
    grant: { key: `tool:${name}`, label: `\`${name}\`` },
  }
}
