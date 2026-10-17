import { afterEach, expect, test } from 'bun:test'
import { createServer } from 'node:net'
import type { AddressInfo, Socket } from 'node:net'
import { INSTALLER_DRAIN_MS, installerTestProcess } from './installer-test-process.ts'

const children = new Set<ReturnType<typeof installerTestProcess>>()
const fixtures = new Set<() => Promise<void>>()
afterEach(async () => {
  await Promise.all([...fixtures].map(dispose => dispose()))
  fixtures.clear()
  const results = await Promise.all([...children].map(child => child.stop('test-aborted')))
  children.clear()
  for (const result of results) expect(result.cleanupErrors).toEqual([])
}, 2 * INSTALLER_DRAIN_MS + 5_000)

test('a completed installer child drains Unicode output without a timeout or signal', async () => {
  const child = installerTestProcess([process.execPath, '-e', 'process.stdout.write("測試 ñ"); process.stderr.write("diagnostic")'])
  children.add(child)
  const result = await child.done
  expect(result).toMatchObject({
    code: 0, signal: null, timedOut: false, aborted: false, stdout: '測試 ñ', stderr: 'diagnostic', cleanupErrors: [],
  })
  expect(await child.stop('test-aborted')).toBe(result)
}, 2 * INSTALLER_DRAIN_MS + 5_000)

test('the watchdog is reported independently of the exit status', async () => {
  const child = installerTestProcess([process.execPath, '-e', 'setInterval(() => {}, 1000)'], { timeoutMs: 100 })
  children.add(child)
  const result = await child.done
  expect(result.timedOut).toBe(true)
  expect(result.aborted).toBe(false)
  expect(result.cleanupErrors).toEqual([])
}, 2 * INSTALLER_DRAIN_MS + 5_000)

test('test-abort cleanup stops and drains the descendant holding its parent output', async () => {
  let ready!: () => void
  const readiness = new Promise<void>((resolve) => { ready = resolve })
  let connection: Socket | undefined
  let closed: Promise<void> = Promise.resolve()
  const socketErrors: string[] = []
  const server = createServer((socket) => {
    connection = socket
    closed = new Promise(resolve => socket.once('close', () => resolve()))
    socket.on('error', (error: NodeJS.ErrnoException) => { if (error.code !== 'ECONNRESET') socketErrors.push(error.message) })
    socket.resume()
  })
  let child: ReturnType<typeof installerTestProcess> | undefined
  let disposal: Promise<void> | undefined
  const dispose = (): Promise<void> => disposal ??= (async () => {
    connection?.end()
    await closed
    if (child !== undefined) await child.stop('test-aborted')
    await new Promise<void>(resolve => server.close(() => resolve()))
  })()
  fixtures.add(dispose)
  try {
    await new Promise<void>((resolve, reject) => { server.once('error', reject); server.listen(0, '127.0.0.1', resolve) })
    const port = (server.address() as AddressInfo).port
    // The fixture's own channel releases a descendant even if tree teardown regresses.
    const descendant = `
      const socket = require('node:net').connect(${port}, '127.0.0.1', () => process.stdout.write('descendant-ready\\n'));
      socket.on('end', () => process.exit(0));
    `
    const parent = `
      require('node:child_process').spawn(process.execPath, ['-e', ${JSON.stringify(descendant)}], { stdio: 'inherit' });
      setInterval(() => {}, 1000);
    `
    child = installerTestProcess([process.execPath, '-e', parent], {
      timeoutMs: 30_000,
      onStdout: (text) => { if (text.includes('descendant-ready\n')) ready() },
    })
    children.add(child)
    await readiness
    const result = await child.stop('test-aborted')
    expect(result.aborted).toBe(true)
    expect(result.timedOut).toBe(false)
    expect(result.stdout).toBe('descendant-ready\n')
    // A surviving descendant retains its inherited pipe and produces an explicit drain error.
    expect(result.cleanupErrors).toEqual([])
    await closed
    expect(socketErrors).toEqual([])
    expect(await child.done).toBe(result)
  } finally {
    await dispose()
    fixtures.delete(dispose)
  }
}, 2 * INSTALLER_DRAIN_MS + 35_000)
