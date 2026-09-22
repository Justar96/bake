/** Focused Node tests for the runtime retained by Bake. TUI tests have their own runner. */
import { defineConfig } from 'vitest/config'
import tsconfigPaths from 'vite-tsconfig-paths'
import { standardDecoratorPlugin, vitestExecArgv } from './vitest.shared.ts'

export default defineConfig({
  plugins: [standardDecoratorPlugin(), tsconfigPaths({ projects: ['./tsconfig.base.json'] })],
  test: {
    include: ['packages/*/*/tests/**/*.spec.ts', 'apps/cli/tests/**/*.spec.ts', 'scripts/**/*.spec.ts'],
    exclude: ['**/node_modules/**', '**/lib/**', '**/*.client.spec.ts'],
    pool: 'forks',
    execArgv: vitestExecArgv,
    testTimeout: 30_000,
    hookTimeout: 30_000,
  },
})
