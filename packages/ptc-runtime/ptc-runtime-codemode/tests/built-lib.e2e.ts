import { existsSync } from 'node:fs'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { execa } from 'execa'
import { describe, expect, it } from 'vitest'

/**
 * Keyless built-artifact smoke: plain Node imports the package by name through its exports map,
 * then exercises type stripping, the sibling `lib/worker.js` entry, the installed QuickJS
 * WebAssembly module, bindings, logs, and deadline interruption. Unit tests start `src/worker.ts`;
 * this pins the downstream `lib/index.js` path. It skips when `lib/` is absent.
 */

const pkgDir = fileURLToPath(new URL('..', import.meta.url))
const built = ['lib/index.js', 'lib/worker.js'].every(file => existsSync(join(pkgDir, file)))
  && existsSync(join(pkgDir, '../ptc-runtime/lib/index.js'))

describe.skipIf(!built)('built lib real load path (plain node)', () => {
  it('runs a TypeScript program through lib/index.js and its lib/worker.js entry', async () => {
    // Workers inherit execArgv, and Node refuses `--input-type` in a worker, so the script is a
    // plain `-e` string that wraps its body in an async function.
    const script = `(async () => {
      const { Context } = await import('@deepseek-ai/cordis')
      const { default: CodemodePtcRuntime } = await import('bake-ptc-runtime-codemode')
      const ctx = new Context()
      await ctx.plugin(CodemodePtcRuntime, {})
      const bindings = [{
        global: 'tools',
        functions: {
          double: async args => args.n * 2,
          fail: async () => { throw new Error('denied') },
        },
        errorClass: { name: 'ToolCallError', memberNameProperty: 'toolName' },
      }]
      const result = await ctx.ptcRuntime.run(ctx.ptcRuntime.resolve({
        program: 'const doubled: number = await tools.double({ n: 21 }); console.log("halfway", doubled); let failure; try { await tools.fail({}) } catch (error) { failure = { typed: error instanceof ToolCallError, name: error.name, toolName: error.toolName, message: error.message } } return { doubled, failure };',
        bindings,
      }))
      const loop = await ctx.ptcRuntime.run(ctx.ptcRuntime.resolve({ program: 'while (true) {}', bindings, timeoutMs: 1000 }))
      await ctx.fiber.dispose()
      console.log(JSON.stringify({ result, loop: loop.error }))
    })().catch(error => { console.error(error); process.exitCode = 1 })`
    const { exitCode, stdout, stderr } = await execa(process.execPath, ['-e', script], {
      cwd: pkgDir,
      stdin: 'ignore',
      timeout: 55_000,
      killSignal: 'SIGKILL',
      reject: false,
    })

    expect(exitCode, `stderr:\n${stderr}`).toBe(0)
    const lastLine = stdout.trim().split('\n').at(-1) ?? ''
    const { result, loop } = JSON.parse(lastLine) as {
      result: { value?: unknown; logs: string[]; error?: unknown }
      loop?: { kind: string; message: string }
    }
    expect(result.error).toBeUndefined()
    expect(result.value).toEqual({
      doubled: 42,
      failure: { typed: true, name: 'ToolCallError', toolName: 'fail', message: 'denied' },
    })
    expect(result.logs).toContain('halfway 42')
    expect(loop).toEqual({ kind: 'timeout', message: 'execution deadline reached (1000ms)' })
  })
})
