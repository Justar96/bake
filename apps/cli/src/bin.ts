#!/usr/bin/env node
/**
 * Command-line entry for dsh.
 *
 * Only Node built-ins load before the module compile cache is on. The
 * dispatch, and the profile graph behind it, are imported afterwards from
 * `cli.ts`, so they compile from, or into, the cache.
 * @module @deepseek-ai/dsh/bin
 */

/* v8 ignore file -- bin.spec.ts exercises this self-executing entry in Node. */

import { restartWithDiagnostics } from './diagnostic-launch.ts'
import { enableCompileCache, flushCompileCache } from './compile-cache.ts'

if (import.meta.main) {
  await restartWithDiagnostics()
  enableCompileCache()
  const { runCli } = await import('./cli.ts')
  await runCli()
  // A profile has booted and handed the process to its app, or a one-shot
  // mode has finished: most modules are loaded.
  flushCompileCache()
}
