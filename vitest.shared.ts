import ts from 'typescript'
import tsconfigPaths from 'vite-tsconfig-paths'
import { defineConfig } from 'vitest/config'

const decoratorSyntax = /^\s*@[A-Za-z_$][\w$]*/m

/**
 * Worker arguments that keep Node's process-wide Web Storage out of test workers, so no
 * test reads or writes storage shared by the whole run.
 * Node lists the positive spelling in `allowedNodeEnvironmentFlags` for this negatable flag.
 */
export const vitestExecArgv = process.allowedNodeEnvironmentFlags.has('--webstorage') ? ['--no-webstorage'] : []

/**
 * Transform standard TypeScript decorators before Vite's default parser sees source files.
 * @returns a pre-transform Vite plugin shared by source-mode test configurations.
 */
export function standardDecoratorPlugin() {
  return {
    name: 'dsh-standard-decorators',
    enforce: 'pre' as const,
    transform(code: string, id: string) {
      const file = id.split('?', 1)[0]!
      if (!/\.[cm]?tsx?$/.test(file) || !decoratorSyntax.test(code)) return
      const result = ts.transpileModule(code, {
        fileName: file,
        compilerOptions: {
          target: ts.ScriptTarget.ES2024,
          module: ts.ModuleKind.ESNext,
          jsx: file.endsWith('x') ? ts.JsxEmit.ReactJSX : undefined,
          sourceMap: true,
        },
      })
      return {
        code: result.outputText
          .replace(
            /^(\s*)(__esDecorate\()/gmu,
            '$1/* v8 ignore next -- compiler-synthetic decorator accessors have no source behavior */ $2',
          )
          .replace(/\n?\/\/# sourceMappingURL=.*$/u, '\n'),
        map: result.sourceMapText,
      }
    },
  }
}

/** What one runtime suite chooses for itself. */
export interface RuntimeSuite {
  readonly include: string[]
  /** Excluded beyond dependencies and built `lib/` output. */
  readonly exclude?: string[]
  /** Run after the proxy reset every suite starts with. */
  readonly setupFiles?: string[]
  readonly testTimeout: number
  readonly hookTimeout: number
}

/**
 * A Node suite over workspace source: the workspace's path aliases and
 * decorators, forked workers without process-wide Web Storage, and no proxy
 * variables from the machine.
 * @param suite - the files it runs, and its own setup and bounds.
 * @returns the Vitest configuration.
 */
export function runtimeSuite(suite: RuntimeSuite) {
  return defineConfig({
    plugins: [standardDecoratorPlugin(), tsconfigPaths({ projects: ['./tsconfig.base.json'] })],
    test: {
      include: suite.include,
      exclude: ['**/node_modules/**', '**/lib/**', ...suite.exclude ?? []],
      pool: 'forks',
      execArgv: vitestExecArgv,
      setupFiles: ['./scripts/test-proxy-environment.ts', ...suite.setupFiles ?? []],
      testTimeout: suite.testTimeout,
      hookTimeout: suite.hookTimeout,
    },
  })
}
