/**
 * Writer-lock holder for the cross-process lock specs. `current` holds the
 * lock through `withFileLock`; `created` creates the lock file the way
 * releases before the kernel-held protocol did. Either prints `holding` once
 * it owns the lock and releases when its stdin closes.
 * Usage: `node --import tsx/esm lock-holder.ts <target> current|created`.
 */

import { rm, writeFile } from 'node:fs/promises'
import { withFileLock } from '../../src/index.ts'

const [target, protocol] = process.argv.slice(2)
if (target === undefined || (protocol !== 'current' && protocol !== 'created')) {
  throw new Error('usage: lock-holder.ts <target> current|created')
}
const stdinClosed = new Promise<void>((resolve) => { process.stdin.once('end', resolve).resume() })

if (protocol === 'created') {
  const lockPath = `${target}.lock`
  await writeFile(lockPath, `${process.pid}\n`, { mode: 0o600, flag: 'wx' })
  process.stdout.write('holding\n')
  await stdinClosed
  await rm(lockPath, { force: true })
} else {
  await withFileLock(target, async () => {
    process.stdout.write('holding\n')
    await stdinClosed
  })
}
