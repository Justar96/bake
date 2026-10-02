/**
 * Model-facing Consumer of the `ctx.shell` capability seam. Background calls
 * register process handles with `ctx.jobs`; their work uses job cancellation
 * rather than the tool-call signal after an id is returned.
 *
 * TODO(permissions): deployment policy belongs in `tools/pre-execute` and
 * sandboxing executors; see docs/architecture.md § Where new behavior goes.
 * @module @deepseek-ai/dsh-tool-bash
 */

import type { Context } from '@deepseek-ai/cordis'
import z from '@deepseek-ai/schemastery'
import { isAbsolute, sep } from 'node:path'
import { defineTool, TOOL_ABORTED } from '@deepseek-ai/dsh-tools'
import type { GenericCallView, TerminalCallView, ToolExecution, ToolResult, ToolResultView } from '@deepseek-ai/dsh-tools'
import { HarnessError } from '@deepseek-ai/dsh-llm'
import type { Agent } from '@deepseek-ai/dsh-agent'
import type {} from '@deepseek-ai/dsh-jobs'
import type {} from '@deepseek-ai/dsh-user-approval'
import type {} from '@deepseek-ai/dsh-shell-env'
import type { SandboxExecutionPolicy, SandboxMode } from '@deepseek-ai/dsh-sandbox'
import { ESCALATION_TARGETS, approveEscalation, validateEscalationArgs, isNoOpEscalation } from '@deepseek-ai/dsh-sandbox'
import type { SandboxPolicyService } from '@deepseek-ai/dsh-sandbox-policy'
import { DSH_ENV_PREFIX } from '@deepseek-ai/dsh-shell'
import type { ShellRunResult } from '@deepseek-ai/dsh-shell'
import { processJob } from './background.ts'
import { openChangeReport, shellChangesOf, terminalChanges, withRunningJobs } from '@deepseek-ai/dsh-shell-change-report'
import { parseExitStatus, renderProcessRead, renderResult } from './render.ts'

export const name = 'tool-bash'
export const inject = ['tools', 'shell', 'shellEnv']

/** Configuration for the bash tool. */
export interface Config {
  /** Expose `run_in_background` (default true); disabled calls are also rejected. */
  enableRunInBackground?: boolean
  /**
   * Show the workspace files a foreground command changed under its output
   * (default true). Display only: the model's result is the same either way.
   */
  changeReport?: boolean
}

/** Runtime configuration schema for the bash tool plugin. */
export const Config: z<Config> = z.object({
  enableRunInBackground: z.boolean().default(true),
  changeReport: z.boolean().default(true),
})

/** Parsed tool args; execute validates value constraints absent from ParameterSchemaSpec. */
interface BashToolArgs {
  command: string
  /** Display-only summary; a missing or blank one falls back to the command. */
  description?: string
  timeoutMs?: number
  /** Undeclared spellings of `timeoutMs`; see {@link requestedTimeoutMs}. */
  timeout?: unknown
  timeout_ms?: unknown
  workdir?: string
  run_in_background?: boolean
  sandbox_permissions?: string
  /** Optional for an omitted or repeated effective mode; widening requires a non-empty reason. */
  justification?: string
}

/** Undeclared timeout spellings, in the order they are consulted after `timeoutMs`. */
const TIMEOUT_ALIASES = ['timeout_ms', 'timeout'] as const

/**
 * The requested timeout in milliseconds. Models used to other harnesses send
 * `timeout` or `timeout_ms`, often as a numeric string. The parameter root
 * admits undeclared keys, so ignoring them would kill a long command at the
 * default timeout while the model believes it asked for more.
 * @param args - schema-validated arguments; `timeoutMs` wins over an alias.
 * @returns the timeout to request, or `undefined` for the executor default.
 */
function requestedTimeoutMs(args: BashToolArgs): number | undefined {
  if (args.timeoutMs !== undefined) return args.timeoutMs
  for (const key of TIMEOUT_ALIASES) {
    const value = args[key]
    if (value === undefined || value === null) continue
    const parsed = typeof value === 'string' && value.trim() !== '' ? Number(value) : value
    if (typeof parsed !== 'number' || !Number.isFinite(parsed) || parsed <= 0) {
      throw new Error(`invalid ${key}: expected a positive number of milliseconds for timeoutMs, got ${JSON.stringify(value)}`)
    }
    return parsed
  }
  return undefined
}

function validateBashArgs(args: BashToolArgs, effectiveMode: SandboxMode | undefined): void {
  if (args.command.trim().length === 0) {
    throw new Error('invalid command: expected a non-empty string')
  }
  if (args.timeoutMs !== undefined && (!Number.isFinite(args.timeoutMs) || args.timeoutMs <= 0)) {
    throw new Error(`invalid timeoutMs: expected a positive number, got ${JSON.stringify(args.timeoutMs)}`)
  }
  const requested = args.sandbox_permissions
  if (requested !== undefined && effectiveMode !== undefined && isNoOpEscalation(effectiveMode, requested)) return
  const justification = args.sandbox_permissions === undefined && args.justification?.trim() === ''
    ? undefined
    : args.justification
  validateEscalationArgs(args.sandbox_permissions, justification)
}

/**
 * Model-facing `bash` description, the only place the tool's guidance lives:
 * no system-prompt section repeats it. It varies only with plugin config and
 * executor capability, never per turn, so the request prefix stays cacheable.
 * The sandbox paragraph appears only with a confining executor, the only kind
 * that can report a denial; whether this session may request approval at all
 * is stated by the approval policy's runtime context.
 */
function bashDescription(backgroundEnabled: boolean, escalationModes: readonly SandboxMode[]): string {
  const background = backgroundEnabled
    ? 'Run long builds and tests with `run_in_background`, which returns a job id at once; read output with `job_output` and stop with `job_kill`.'
    : 'Background execution is not available, so a command must finish within its timeout.'
  // The registry's built-ins; DSH_SHELL=1 only marks the process and tells the model nothing.
  const base = 'Run a command with `bash -c` and return its stdout and stderr. '
    + 'Each call starts a fresh shell, so directory changes and variables do not carry over to later calls. '
    + 'Avoid filesystem-wide `find` scans. '
    + 'A non-zero exit is reported in the result as `[exit code: N]`, not as a tool error. '
    + 'Long output is truncated to its tail, and the full output is saved to a file named in the result when possible. '
    + `\`$${DSH_ENV_PREFIX}HOME\` is the harness home directory and \`$${DSH_ENV_PREFIX}SESSION_ID\` is this session's id. `
    + background
  if (escalationModes.length === 0) return base
  return base + ' Commands may run in a file sandbox; trying one it might block is safe. '
    + 'A blocked file operation reports `[sandbox: file access denied under <mode> mode]`: '
    + 'a policy denial, not a bug in the command, so do not work around it. '
    + 'When a wider mode would let a denied command succeed, retry that same command once in the same turn '
    + 'with the narrowest sufficient `sandbox_permissions` and a `justification`. '
    + 'That retry itself asks the user for approval, so there is no need to ask in chat first. '
    + 'Request a wider mode up front only when this session already denied the same access. '
    + 'A rejection is final for that command: stop and explain. Other commands can still run or escalate.'
}

/**
 * Present foreground calls as terminals and background starts as generic cards.
 * The command remains the title on both paths; foreground cwd is passed through
 * for the bridge to resolve, while background descriptions remain card content.
 * The description is optional display metadata: a missing one costs the model
 * nothing, so it is never worth a refused call.
 */
type BashCallArgs = { command: string; description?: string; workdir?: string; run_in_background?: boolean }

/** The non-blank description, if the model supplied one. */
function displayDescription(args: BashCallArgs): string | undefined {
  const text = typeof args.description === 'string' ? args.description.trim() : ''
  return text.length > 0 ? args.description : undefined
}

function presentBashCall(args: BashCallArgs): GenericCallView | TerminalCallView {
  if (args.run_in_background === true) {
    return {
      card: 'generic',
      title: args.command,
      kind: 'execute',
      rawInput: args.command,
      content: [{ type: 'text', text: displayDescription(args) ?? args.command }],
    }
  }
  const description = displayDescription(args)
  return {
    card: 'terminal',
    title: args.command,
    ...description === undefined ? {} : { description },
    ...args.workdir !== undefined ? { cwd: args.workdir } : {},
  }
}

/**
 * Present completed foreground output as a terminal; background acknowledgements
 * and execution errors use generic fenced output without an exit-status pill.
 */
function presentBashResult(args: unknown, result: ToolResult): ToolResultView | undefined {
  const block = result.content.length === 1 ? result.content[0] : undefined
  if (block === undefined || block.type !== 'text') return undefined
  const raw = block.text
  const isBackground = typeof args === 'object' && args !== null && (args as { run_in_background?: unknown }).run_in_background === true
  // Background acknowledgements and errors have no terminal exit status.
  if (isBackground || result.isError) {
    return { card: 'generic', content: [{ type: 'text', text: `\`\`\`console\n${raw.replace(/\n+$/, '')}\n\`\`\`` }] }
  }
  // The exit marker becomes the card's exit pill, so it leaves the output body.
  const { body, ...exit } = parseExitStatus(raw)
  const changes = terminalChanges(shellChangesOf(result.meta))
  return { card: 'terminal', output: body, ...exit, ...changes === undefined ? {} : { changes } }
}

/**
 * Resolve an explicit workdir first, making a relative one session-workspace-relative;
 * otherwise use the filesystem identity of the session cwd and leave executor
 * defaulting as the fallback. A resolved sandbox-policy root wins so workdir
 * and confinement use the exact same per-call identity.
 */
function resolveWorkdir(
  modelWorkdir: string | undefined,
  exec: { agent?: Agent },
  policyWorkspaceRoot?: string,
): string | undefined {
  const headerCwd = exec.agent?.session.header.cwd
  const sessionCwd = policyWorkspaceRoot ?? headerCwd
  if (modelWorkdir === undefined) return sessionCwd
  if (sessionCwd !== undefined && !isAbsolute(modelWorkdir)) {
    return `${sessionCwd}${sep}${modelWorkdir}`
  }
  return modelWorkdir
}

/** Detach the executor DTO from readonly Service Definition types into plain JSON data. */
function canonicalBashResult(result: ShellRunResult) {
  const output = (stream: ShellRunResult['stdout']) => ({
    text: stream.text,
    truncated: stream.truncated,
    ...stream.spillPath !== undefined ? { spillPath: stream.spillPath } : {},
  })
  return {
    exitCode: result.exitCode,
    signal: result.signal,
    timedOut: result.timedOut,
    aborted: result.aborted,
    timeoutMs: result.timeoutMs,
    stdout: output(result.stdout),
    stderr: output(result.stderr),
    ...result.sandbox !== undefined ? {
      sandbox: {
        mode: result.sandbox.mode,
        denied: result.sandbox.denied,
        ...result.sandbox.enforcement !== undefined ? { enforcement: result.sandbox.enforcement } : {},
        ...result.sandbox.runnerFailed !== undefined ? { runnerFailed: result.sandbox.runnerFailed } : {},
      },
    } : {},
  }
}

/** Canonical background-handle properties shared by the bash output union. */
const BACKGROUND_OUTPUT_PROPERTIES = {
  kind: { type: 'string', required: true, const: 'background' },
  jobId: { type: 'string', required: true },
} as const

export function apply(ctx: Context, config: Config = {}): void {
  const backgroundEnabled = config.enableRunInBackground ?? true
  const changeReport = config.changeReport ?? true
  const defaultMode = ctx.shell.sandboxMode
  const escalationModes: readonly SandboxMode[] = defaultMode === undefined ? [] : ESCALATION_TARGETS
  const sandboxPolicy: SandboxPolicyService | undefined = defaultMode === undefined ? undefined : ctx.get('sandboxPolicy')
  if (defaultMode !== undefined && sandboxPolicy === undefined) {
    throw new Error('tool-bash: the mounted bash executor confines but ctx.sandboxPolicy is missing')
  }
  /** Resolve the complete standing policy for this call when a confining executor is mounted. */
  const resolveSandboxPolicy = (exec: ToolExecution): SandboxExecutionPolicy | undefined =>
    sandboxPolicy?.resolve(exec.agent === undefined ? {} : { session: exec.agent.session })

  /**
   * Resolve a sandbox-escalation request through `ctx.approval` BEFORE
   * anything executes, delegating the shared fail-closed sequence (strict
   * widening, channel resolution, outcome mapping) to
   * {@link approveEscalation}. This tool contributes only the composition
   * guard (the fields are unadvertised without a sandboxing executor, yet
   * schema validation checks advertised keys only, so an unadvertised
   * `sandbox_permissions` still reaches execute) and the approval
   * ingredients. The shared policy resolver is required whenever the executor
   * advertises confinement, so a split composition fails at tool-plugin load.
   */
  const approveBashEscalation = (
    mode: string,
    justification: string,
    exec: ToolExecution,
    standingPolicy: SandboxExecutionPolicy | undefined,
  ): Promise<SandboxMode> => {
    if (escalationModes.length === 0) {
      throw new Error('sandbox_permissions is not available in this composition (no sandboxing executor to escalate)')
    }
    const effectiveMode = (standingPolicy as SandboxExecutionPolicy).mode
    return approveEscalation(
      { requestedMode: mode, justification, effectiveMode, subject: 'command' },
      {
        approver: ctx.get('approval'),
        agent: exec.agent,
        callId: exec.callId,
        toolName: 'bash',
        signal: exec.signal,
      },
    )
  }

  ctx.tools.register(defineTool({
    name: 'bash',
    description: bashDescription(backgroundEnabled, escalationModes),
    parameters: {
      command: { type: 'string', required: true, description: 'The bash command to execute.' },
      description: { type: 'string', description: 'Short summary of what the command does, shown to the user.' },
      timeoutMs: { type: 'number', description: 'Timeout in milliseconds, capped at the maximum; the command is killed when it expires.' },
      workdir: { type: 'string', description: 'Directory to run this command in. Defaults to your working directory; a relative path resolves against it.' },
      ...backgroundEnabled ? {
        run_in_background: { type: 'boolean' as const, description: 'Run in the background, with no timeout.' },
      } : {},
      ...escalationModes.length > 0 ? {
        sandbox_permissions: {
          type: 'string' as const,
          enum: [...escalationModes],
          description: 'Wider sandbox mode for retrying a denied command.',
        },
        justification: {
          type: 'string' as const,
          description: 'One sentence telling the user why this command needs wider access.',
        },
      } : {},
    },
    output: {
      schema: {
        oneOf: [
          {
            type: 'object',
            additionalProperties: false,
            properties: BACKGROUND_OUTPUT_PROPERTIES,
          },
          {
            type: 'object',
            additionalProperties: false,
            properties: {
              kind: { type: 'string', required: true, const: 'foreground' },
              exitCode: { required: true, oneOf: [{ type: 'integer' }, { type: 'null' }] },
              signal: { required: true, oneOf: [{ type: 'string' }, { type: 'null' }] },
              timedOut: { type: 'boolean', required: true },
              aborted: { type: 'boolean', required: true },
              timeoutMs: { type: 'number', required: true },
              stdout: {
                type: 'object',
                additionalProperties: false,
                required: true,
                properties: {
                  text: { type: 'string', required: true },
                  truncated: { type: 'boolean', required: true },
                  spillPath: { type: 'string' },
                },
              },
              stderr: {
                type: 'object',
                additionalProperties: false,
                required: true,
                properties: {
                  text: { type: 'string', required: true },
                  truncated: { type: 'boolean', required: true },
                  spillPath: { type: 'string' },
                },
              },
              sandbox: {
                type: 'object',
                additionalProperties: false,
                properties: {
                  mode: { type: 'string', required: true },
                  denied: { type: 'boolean', required: true },
                  enforcement: { type: 'string' },
                  runnerFailed: { type: 'boolean' },
                },
              },
            },
          },
        ],
      },
      render: (_args, value) => [{
        type: 'text',
        text: value.kind === 'background'
          ? `started background job ${value.jobId}`
          : renderResult(value as { kind: 'foreground' } & ShellRunResult, escalationModes),
      }],
    },
    async execute(args: BashToolArgs, exec) {
      // Description is display metadata; workdir defaults to the caller's session.
      const standingPolicy = resolveSandboxPolicy(exec)
      validateBashArgs(args, standingPolicy?.mode)
      const timeoutMs = requestedTimeoutMs(args)
      const approvedMode = args.sandbox_permissions !== undefined && args.justification !== undefined
        ? await approveBashEscalation(args.sandbox_permissions, args.justification, exec, standingPolicy)
        : undefined
      const policy = approvedMode === undefined
        ? standingPolicy
        : { ...(standingPolicy as SandboxExecutionPolicy), mode: approvedMode }
      const workdir = resolveWorkdir(args.workdir, exec, standingPolicy?.workspaceRoot)
      const dshEnv = ctx.shellEnv.collect(exec)
      const request = {
        command: args.command,
        ...workdir !== undefined ? { workdir } : {},
        ...timeoutMs !== undefined ? { timeoutMs } : {},
        dshEnv,
        ...policy !== undefined ? { sandboxPolicy: policy } : {},
      }
      if (args.run_in_background === true) {
        // Undeclared keys are allowed, so schema omission also needs enforcement.
        if (!backgroundEnabled) {
          throw new Error('run_in_background is disabled for this deployment (enableRunInBackground: false)')
        }
        const jobs = ctx.get('jobs')
        if (jobs === undefined) {
          throw new Error('background jobs unavailable: load @deepseek-ai/dsh-jobs and @deepseek-ai/dsh-tool-jobs')
        }
        // The caller owns cancellation until ctx.jobs commits detached ownership.
        if (exec.signal.aborted) {
          const error = new HarnessError('tool call aborted', TOOL_ABORTED)
          error.name = 'AbortError'
          throw error
        }
        // Task preflight finishes before the starter can spawn a process.
        const id = jobs.start({
          kind: 'bash',
          label: args.command,
          ...exec.agent ? { owner: exec.agent } : {},
          run: () => processJob(
            signal => ctx.shell.start(ctx.shell.resolve({ ...request, signal })),
            proc => renderProcessRead(proc.readOutput(), proc.sandbox, escalationModes),
          ),
        })
        return { kind: 'background' as const, jobId: id }
      }
      // After approval, so a long approval wait never widens the window.
      const window = changeReport ? await openChangeReport(ctx, exec, policy, workdir) : undefined
      try {
        const result = await ctx.shell.run(ctx.shell.resolve({
          ...request,
          signal: exec.signal,
          // Display-only: the runtime shows the tail under the running call.
          onOutput: (tail) => { exec.reportProgress({ output: tail }) },
        }))
        if (result.aborted) {
          const error = new HarnessError('tool call aborted', TOOL_ABORTED)
          error.name = 'AbortError'
          throw error
        }
        const changes = await window?.finish(exec.signal)
        if (changes !== undefined) exec.presentResultMeta({ shellChanges: withRunningJobs(ctx, exec, changes) })
        return { kind: 'foreground' as const, ...canonicalBashResult(result) }
      } finally {
        await window?.release()
      }
    },
    presentCall: presentBashCall,
    presentResult: presentBashResult,
  }))
}
