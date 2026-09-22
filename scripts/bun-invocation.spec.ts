import { describe, expect, it } from 'vitest'
import { bunInvocation } from './bun-invocation.ts'

describe('bun invocation', () => {
  it.each([
    '/tools/bun',
    '/tools/with spaces/$bun;/bun',
    String.raw`C:\Program Files\工具\$bun;\bun.exe`,
  ])('runs the executable entrypoint %j directly', (entrypoint) => {
    expect(bunInvocation(['run', 'build'], { npm_execpath: entrypoint })).toEqual({
      command: entrypoint,
      args: ['run', 'build'],
    })
  })

  it.each([undefined, ''])('rejects an unavailable lifecycle entrypoint', (entrypoint) => {
    expect(() => bunInvocation([], { npm_execpath: entrypoint }))
      .toThrow('npm_execpath is unavailable; invoke the script through bun run')
  })
})
