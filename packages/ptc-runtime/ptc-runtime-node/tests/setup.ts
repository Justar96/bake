import { spawnSync } from 'node:child_process'
import { mkdtemp, rm } from 'node:fs/promises'
import { homedir } from 'node:os'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { Context } from '@deepseek-ai/cordis'
import { onTestFinished } from 'vitest'
import SessionStore from '@deepseek-ai/dsh-session'
import FileSystem from '@deepseek-ai/dsh-fs-local'
import Subprocess from '@deepseek-ai/dsh-subprocess-local'
import Sandbox from '@deepseek-ai/dsh-sandbox-local'
import SandboxPolicy from '@deepseek-ai/dsh-sandbox-policy'
import SessionProjections from '@deepseek-ai/dsh-session-projection'
import { SandboxUnavailableError, type SandboxMode } from '@deepseek-ai/dsh-sandbox'
import NodeRuntime from '../src/index.ts'
import type { Config } from '../src/index.ts'

export async function mountRuntime(ctx: Context, config: Config = {}, policy: { mode?: SandboxMode; workspaceRoot?: string } = {}) {
  onTestFinished(async () => { await ctx.fiber.dispose() })
  if (!ctx.get('sessions')) await ctx.plugin(SessionStore)
  if (!ctx.get('fs')) await ctx.plugin(FileSystem)
  if (!ctx.get('subprocess')) await ctx.plugin(Subprocess)
  if (!ctx.get('sandbox')) await ctx.plugin(Sandbox, {})
  if (!ctx.get('sessionProjections')) await ctx.plugin(SessionProjections)
  if (!ctx.get('sandboxPolicy')) await ctx.plugin(SandboxPolicy, { mode: 'danger-full-access', ...policy })
  await ctx.plugin(NodeRuntime, config)
  return ctx.ptcRuntime as NodeRuntime
}

/**
 * Explain why workspace-write runs cannot load this source checkout, or answer undefined when they can.
 * Source-mode runs import the checkout's `.ts` bootstrap inside the sandbox, and the bwrap backend mounts a
 * private tmpfs over `/tmp` by design, so a checkout under `/tmp` (a scratch worktree) is invisible there.
 * @returns a precise skip reason for a missing prerequisite; other probe failures throw.
 */
export async function confinedCheckoutUnavailable(): Promise<string | undefined> {
  const source = fileURLToPath(import.meta.resolve('@deepseek-ai/dsh-subprocess/package.json'))
  const root = await mkdtemp(join(homedir(), '.dsh-sandbox-checkout-probe-'))
  const ctx = new Context()
  try {
    await ctx.plugin(Sandbox, {})
    const check = `process.stdout.write(require("node:fs").existsSync(${JSON.stringify(source)}) ? "visible" : "hidden")`
    const [command, ...args] = (await ctx.sandbox.confine([process.execPath, '-e', check], { mode: 'workspace-write', workspaceRoot: root })).argv
    const probe = spawnSync(command!, args, { encoding: 'utf8' })
    if (probe.stdout === 'visible') return undefined
    if (probe.stdout === 'hidden') {
      return `workspace-write confinement hides this source checkout (${source}); bwrap mounts a private /tmp, so run from a checkout outside /tmp`
    }
    throw new Error(`workspace-write checkout probe failed (status ${probe.status}): ${probe.error?.message ?? probe.stderr}`)
  } catch (error: unknown) {
    if (error instanceof SandboxUnavailableError) return `no enforcing sandbox backend: ${error.message}`
    throw error
  } finally {
    await ctx.fiber.dispose()
    await rm(root, { recursive: true, force: true })
  }
}
