import { EventEmitter } from 'node:events'
import { describe, expect, it } from 'vitest'
import { tolerateLostTerminal, type HangupStdio } from '../src/terminal-hangup.ts'

function lostTerminal(): Record<'stdin' | 'stdout' | 'stderr', EventEmitter> {
  return { stdin: new EventEmitter(), stdout: new EventEmitter(), stderr: new EventEmitter() }
}

const eio = (): Error => Object.assign(new Error('write EIO'), { code: 'EIO' })

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
