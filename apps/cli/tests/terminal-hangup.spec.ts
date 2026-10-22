import { EventEmitter } from 'node:events'
import { describe, expect, it, onTestFinished, vi } from 'vitest'
import { tolerateLostTerminal, watchTerminalHangup, type HangupStdio } from '../src/terminal-hangup.ts'
import { createProcessShutdown } from '../src/process-shutdown.ts'

function lostTerminal(): Record<'stdin' | 'stdout' | 'stderr', EventEmitter> {
  return { stdin: new EventEmitter(), stdout: new EventEmitter(), stderr: new EventEmitter() }
}

const eio = (): Error => Object.assign(new Error('write EIO'), { code: 'EIO' })

describe('watchTerminalHangup', () => {
  function terminals() {
    const streams = lostTerminal()
    for (const stream of Object.values(streams)) Object.assign(stream, { isTTY: true })
    return streams
  }

  it('joins stream loss before SIGHUP into one shutdown that awaits disposal', async () => {
    const streams = terminals()
    const stdio = streams as unknown as HangupStdio
    const disposed = Promise.withResolvers<undefined>()
    const dispose = vi.fn(() => disposed.promise)
    const exit = vi.fn()
    const complete = vi.fn()
    const shutdown = createProcessShutdown(dispose, exit, complete)
    const hangup = () => {
      tolerateLostTerminal(stdio)
      shutdown.hangup(129)
    }
    const stop = watchTerminalHangup(hangup, stdio)
    onTestFinished(stop)
    try {
      // Force the ordering the kernel and signal dispatcher do not guarantee.
      streams.stdout.emit('error', eio())
      await Promise.resolve()
      hangup()
      streams.stderr.emit('error', Object.assign(new Error('write EPIPE'), { code: 'EPIPE' }))
      expect(dispose).toHaveBeenCalledOnce()
      expect(exit).not.toHaveBeenCalled()
      disposed.resolve(undefined)
      await shutdown.shutdown(129)
      expect(exit).toHaveBeenCalledExactlyOnceWith(129)
      expect(complete).not.toHaveBeenCalled()
      stop()
      // Signal-time tolerance stays installed after the observation ends.
      expect(() => streams.stdout.emit('error', eio())).not.toThrow()
    } finally {
      disposed.resolve(undefined)
      await shutdown.shutdown(129)
    }
  })

  it('leaves redirected pipes and unrelated terminal errors unhandled', () => {
    const streams = lostTerminal()
    Object.assign(streams.stdin, { isTTY: true })
    const hangup = vi.fn()
    onTestFinished(watchTerminalHangup(hangup, streams as unknown as HangupStdio))

    expect(() => streams.stdout.emit('error', eio())).toThrow('write EIO')
    expect(() => streams.stderr.emit('error', eio())).toThrow('write EIO')
    expect(() => streams.stdin.emit('error', new Error('unrelated'))).toThrow('unrelated')
    expect(hangup).not.toHaveBeenCalled()
  })

  it('preserves existing error listeners and removes only its own watchers', () => {
    const streams = terminals()
    const existing = vi.fn()
    streams.stdout.on('error', existing)
    const hangup = vi.fn()
    const stop = watchTerminalHangup(hangup, streams as unknown as HangupStdio)
    onTestFinished(stop)
    const error = new Error('unrelated')

    expect(() => streams.stdout.emit('error', error)).not.toThrow()
    expect(existing).toHaveBeenCalledExactlyOnceWith(error)
    expect(hangup).not.toHaveBeenCalled()
    stop()
    stop()
    expect(streams.stdout.listeners('error')).toEqual([existing])
    expect(streams.stderr.listenerCount('error')).toBe(0)
    expect(streams.stdin.listenerCount('error')).toBe(0)
    expect(() => streams.stderr.emit('error', eio())).toThrow('write EIO')
  })

  it('recognizes repeated terminal-loss notifications using the captured terminal identity', () => {
    const streams = terminals()
    const hangup = vi.fn()
    onTestFinished(watchTerminalHangup(hangup, streams as unknown as HangupStdio))
    Object.assign(streams.stdout, { isTTY: false })

    streams.stdout.emit('error', eio())
    streams.stdout.emit('error', Object.assign(new Error('write EPIPE'), { code: 'EPIPE' }))
    streams.stdin.emit('error', eio())
    expect(hangup).toHaveBeenCalledTimes(3)
  })

  it('lets an existing once-listener handle an unrelated error before becoming unhandled', () => {
    const streams = terminals()
    const existing = vi.fn()
    streams.stdout.once('error', existing)
    const hangup = vi.fn()
    onTestFinished(watchTerminalHangup(hangup, streams as unknown as HangupStdio))
    const error = new Error('unrelated')

    expect(() => streams.stdout.emit('error', error)).not.toThrow()
    expect(existing).toHaveBeenCalledExactlyOnceWith(error)
    expect(() => streams.stdout.emit('error', error)).toThrow('unrelated')
    expect(hangup).not.toHaveBeenCalled()
  })

  it('rolls back its listeners when a later stream cannot be acquired', () => {
    const streams = terminals()
    const existing = vi.fn()
    streams.stdout.on('error', existing)
    const failure = new Error('stdin unavailable')
    const stdio = {
      stdout: streams.stdout,
      stderr: streams.stderr,
      get stdin(): never { throw failure },
    }

    expect(() => watchTerminalHangup(vi.fn(), stdio as unknown as HangupStdio)).toThrow(failure)
    expect(streams.stdout.listeners('error')).toEqual([existing])
    expect(streams.stderr.listenerCount('error')).toBe(0)
  })
})

describe('tolerateLostTerminal', () => {
  it('drops stdio errors that would otherwise throw from the emitting write', () => {
    const streams = lostTerminal()
    expect(() => streams.stdout.emit('error', eio())).toThrow('write EIO')

    tolerateLostTerminal(streams as unknown as HangupStdio)

    for (const stream of Object.values(streams)) {
      expect(() => stream.emit('error', eio())).not.toThrow()
    }
  })

  it('adds one listener per stream however often the terminal hangs up', () => {
    const streams = lostTerminal()
    tolerateLostTerminal(streams as unknown as HangupStdio)
    tolerateLostTerminal(streams as unknown as HangupStdio)

    for (const stream of Object.values(streams)) expect(stream.listenerCount('error')).toBe(1)
  })

  it('covers the other streams when one cannot be opened on the lost terminal', () => {
    const stdout = new EventEmitter()
    const stderr = new EventEmitter()
    const stdio = {
      stdout,
      stderr,
      get stdin(): never { throw Object.assign(new Error('TTY initialization failed'), { code: 'ERR_TTY_INIT_FAILED' }) },
    }

    expect(() => { tolerateLostTerminal(stdio as unknown as HangupStdio) }).not.toThrow()
    expect(stdout.listenerCount('error')).toBe(1)
    expect(stderr.listenerCount('error')).toBe(1)
  })
})
