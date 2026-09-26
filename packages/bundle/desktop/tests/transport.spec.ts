/** The bridge's message-port and stdio carriers and its inbound envelope checks. */

import { PassThrough } from 'node:stream'
import { describe, expect, it } from 'vitest'
import type { CoreMessage } from '../src/protocol.ts'
import { decodeCoreLine, openTransport, type ParentPortLike } from '../src/transport.ts'

function handlers() {
  const messages: CoreMessage[] = []
  const invalid: string[] = []
  let closed = 0
  return {
    messages,
    invalid,
    closed: () => closed,
    handlers: {
      onMessage: (m: CoreMessage) => { messages.push(m) },
      onInvalid: (r: string) => { invalid.push(r) },
      onClose: () => { closed++ },
    },
  }
}

describe('decodeCoreLine', () => {
  it('accepts known core messages and refuses everything else', () => {
    expect(decodeCoreLine('{"v":1,"type":"shutdown"}')).toEqual({ v: 1, type: 'shutdown' })
    expect(decodeCoreLine('nope')).toMatch(/^invalid JSON/)
    expect(decodeCoreLine('[]')).toBe('message is not an object')
    expect(decodeCoreLine('{"v":2,"type":"shutdown"}')).toBe('unsupported protocol version 2')
    // A harness-to-core type is not accepted inbound.
    expect(decodeCoreLine('{"v":1,"type":"ready"}')).toBe('unknown message type ready')
  })
})

describe('openTransport', () => {
  it('uses the message port when one exists', () => {
    const sent: unknown[] = []
    let listener: ((event: { data: unknown }) => void) | undefined
    const port: ParentPortLike = {
      on: (_event, l) => { listener = l },
      postMessage: (m) => { sent.push(m) },
    }
    const h = handlers()
    const transport = openTransport(h.handlers, { parentPort: port })
    expect(transport.kind).toBe('port')
    listener?.({ data: '{"v":1,"type":"clock.ping","id":3}' })
    listener?.({ data: { not: 'a string' } })
    transport.send({ v: 1, type: 'clock.pong', id: 3, now_us: 1 })
    expect(h.messages).toEqual([{ v: 1, type: 'clock.ping', id: 3 }])
    expect(h.invalid).toEqual(['port message is not a string'])
    expect(sent).toEqual(['{"v":1,"type":"clock.pong","id":3,"now_us":1}'])
    transport.close()
    transport.send({ v: 1, type: 'clock.pong', id: 4, now_us: 1 })
    expect(sent).toHaveLength(1)
  })

  it('falls back to newline-delimited stdio and reports the end of input', async () => {
    const stdin = new PassThrough()
    const out: string[] = []
    const h = handlers()
    const transport = openTransport(h.handlers, {
      parentPort: undefined,
      stdin,
      stdout: { write: (chunk: string) => out.push(chunk) },
    })
    expect(transport.kind).toBe('stdio')
    stdin.write('{"v":1,"type":"user.message","text":"hi"}\r\n\n{"v":1,')
    stdin.write('"type":"shutdown"}\n')
    transport.send({ v: 1, type: 'error', fatal: false, code: 'X', message: 'y' })
    stdin.end()
    await new Promise(resolve => setImmediate(resolve))
    expect(h.messages.map(m => m.type)).toEqual(['user.message', 'shutdown'])
    expect(out).toEqual(['{"v":1,"type":"error","fatal":false,"code":"X","message":"y"}\n'])
    expect(h.closed()).toBe(1)
  })
})
