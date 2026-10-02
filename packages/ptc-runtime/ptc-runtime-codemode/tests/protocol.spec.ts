import { describe, expect, it } from 'vitest'
import { programFailureText, readWorkerHeader, withWorkerHeader } from '../src/protocol.ts'

describe('worker header', () => {
  it('round-trips the run values and rejects malformed headers', () => {
    const header = { maxOutputBytes: 64, maxMessageBytes: 128, lines: 3, column: 210 }
    expect(readWorkerHeader(withWorkerHeader(header, 'return 1 /*x*/'))).toEqual({ header, code: 'return 1 /*x*/' })
    expect(readWorkerHeader('return 1')).toBeUndefined()
    expect(readWorkerHeader('/*dsh-ptc-codemode:1:2:3*/return 1')).toBeUndefined()
    expect(readWorkerHeader('/*dsh-ptc-codemode:1:2:3:-4*/return 1')).toBeUndefined()
    expect(readWorkerHeader('/*dsh-ptc-codemode:1:2:3:4')).toBeUndefined()
  })
})

describe('programFailureText', () => {
  const layout = { lines: 3, column: 100 }

  it('keeps program frames with program-relative positions and drops runtime frames', () => {
    const stack = [
      'Error: boom',
      '    at inner (codemode.js:1:120)',
      '    at <anonymous> (codemode.js:3:5)',
      '    at __dsh_main__ (codemode.js:9:22)',
      '    at <anonymous> (codemode.js:1:41)',
      '    at map (native)',
    ].join('\n')
    expect(programFailureText({ name: 'Error', message: 'boom', stack }, layout)).toBe([
      'Error: boom',
      '    at inner (<program>:1:21)',
      '    at <anonymous> (<program>:3:5)',
      '    at map (native)',
    ].join('\n'))
  })

  it('handles multi-line messages, missing stacks, and stacks the program replaced', () => {
    expect(programFailureText({ name: 'Error', message: 'a\nb', stack: 'Error: a\nb\n    at f (codemode.js:2:1)' }, layout)).toBe('Error: a\nb\n    at f (<program>:2:1)')
    expect(programFailureText({ message: 'plain' }, layout)).toBe('plain')
    expect(programFailureText({ name: 'Error', message: 7, stack: 7 }, layout)).toBe('7')
    expect(programFailureText({ name: 'Error', message: '', stack: 'Error' }, layout)).toBe('Error')
    expect(programFailureText({ name: 'Error', message: 'x', stack: 'custom' }, layout)).toBe('custom')
  })
})
