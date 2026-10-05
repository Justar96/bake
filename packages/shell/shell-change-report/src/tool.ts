/**
 * What a shell tool needs around one call: a report window that follows the
 * call's file policy, the running-jobs flag, and the terminal card's section.
 * Shared by `tool-bash` and `tool-pwsh`.
 * @module bake-shell-change-report/tool
 */

import type { Context } from '@deepseek-ai/cordis'
import type {} from 'bake-jobs'
import type { SandboxExecutionPolicy } from 'bake-sandbox'
import { beginChangeReport } from './index.ts'
import type { ChangeWindow, ConfineArgv, ShellChanges } from './index.ts'
import type { TerminalChanges, ToolExecution } from 'bake-tools'

/**
 * Open a report window around one call, or none.
 *
 * Only a top-level call with an agent is reported: a nested PTC call carries
 * no metadata. The git reads follow the call's file policy. Under
 * `read-only`, the command cannot write the workspace, so there is nothing to
 * report. A confined `workspace-write` call can rewrite `.git/config`, and
 * `git status` runs the clean filters named there, so its reads run under
 * `read-only` confinement, and without a sandbox provider there is no report.
 * An unconfined call's reads cannot reach anything the command could not.
 * @param ctx - the plugin context, for the optional sandbox provider.
 * @param exec - the call.
 * @param policy - the call's effective file policy, after any approved escalation.
 * @param workdir - where the command runs; the session cwd when omitted.
 * @returns the open window, or `undefined` when the call is not reported.
 */
export async function openChangeReport(
  ctx: Context,
  exec: ToolExecution,
  policy: SandboxExecutionPolicy | undefined,
  workdir: string | undefined,
): Promise<ChangeWindow | undefined> {
  if (exec.parent !== undefined || exec.agent === undefined) return undefined
  const cwd = exec.agent.session.header.cwd ?? workdir
  if (cwd === undefined) return undefined
  let confine: ConfineArgv | undefined
  if (policy !== undefined && policy.mode !== 'danger-full-access') {
    if (policy.mode === 'read-only') return undefined
    const sandbox = ctx.get('sandbox')
    if (sandbox === undefined) return undefined
    const { workspaceRoot, sessionId } = policy
    confine = async (argv, signal) => (await sandbox.confine(argv, {
      mode: 'read-only', workspaceRoot, ...sessionId === undefined ? {} : { sessionId },
    }, signal)).argv
  }
  return beginChangeReport({ workdir: workdir ?? cwd, displayRoot: cwd, signal: exec.signal, ...confine === undefined ? {} : { confine } })
}

/**
 * Mark the report as possibly including other activity when the caller had a
 * background job running: its writes land in the same window.
 */
export function withRunningJobs(ctx: Context, exec: ToolExecution, changes: ShellChanges): ShellChanges {
  const running = ctx.get('jobs')?.list(exec.agent).some(job => job.status === 'running' || job.status === 'stopping') ?? false
  return running ? { ...changes, concurrent: true } : changes
}

/**
 * The terminal card's changes section for a report.
 * @param report - the narrowed `meta.shellChanges`, if any.
 * @returns the section, with each hunk carrying its file's path.
 */
export function terminalChanges(report: ShellChanges | undefined): TerminalChanges | undefined {
  if (report === undefined) return undefined
  return {
    files: report.files.map(({ hunks, ...file }) => ({
      ...file,
      ...hunks === undefined ? {} : { hunks: hunks.map(hunk => ({ path: file.path, ...hunk })) },
    })),
    ...report.omittedFiles === undefined ? {} : { omittedFiles: report.omittedFiles },
    ...report.timedOut === true ? { timedOut: true as const } : {},
    ...report.concurrent === true ? { concurrent: true as const } : {},
  }
}
