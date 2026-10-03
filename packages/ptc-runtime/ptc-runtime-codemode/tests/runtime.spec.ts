import { describe, expect, it } from 'vitest'
import type { PtcRunResult } from '@deepseek-ai/dsh-ptc-runtime'
import { mountRuntime, tools } from './setup.ts'

/** Serialized size of a result's logs plus its completion or diagnostic, as the output budget counts it. */
function outerBytes(result: PtcRunResult): number {
  const tail = result.error === undefined ? result.value : result.error.message
  return Buffer.byteLength(JSON.stringify(result.logs)) + (tail === undefined ? 0 : Buffer.byteLength(JSON.stringify(tail)))
}

describe('QuickJS program execution', () => {
  it('runs erasable TypeScript with typed binding results, rejections and console output', async () => {
    const { run } = await mountRuntime()
    const result = await run({
      program: [
        'interface Doubled { n: number }',
        'const doubled: Doubled = { n: await tools.double({ n: 21 }) as number }',
        'console.log("half", doubled.n, { a: 1 }, [1, 2])',
        'console.error("to logs too")',
        'let failure',
        'try { await tools.fail({}) } catch (error) {',
        '  failure = { typed: error instanceof ToolCallError, name: error.name, toolName: error.toolName, message: error.message, keys: Object.keys(error) }',
        '}',
        'return { doubled, failure }',
      ].join('\n'),
      bindings: tools({
        double: async args => (args as { n: number }).n * 2,
        fail: async () => { throw new Error('denied') },
      }),
    })
    expect(result).toEqual({
      logs: ['half 42 {"a":1} [1,2]', 'to logs too'],
      value: {
        doubled: { n: 42 },
        failure: { typed: true, name: 'ToolCallError', toolName: 'fail', message: 'denied', keys: ['name', 'toolName'] },
      },
    })
  })

  it('exposes only declared bindings, console and standard built-ins in strict mode', async () => {
    const { run } = await mountRuntime()
    const names = ['process', 'require', 'fetch', 'setTimeout', 'Buffer', 'text', 'image', 'exit', 'store', 'load', 'ALL_TOOLS', 'tools', '__dsh_call__', '__dsh_main__']
    const result = await run({
      program: `return { absent: [${names.map(name => `typeof ${name}`).join(', ')}], self: this === undefined, json: JSON.stringify({ ok: [new Map().size, Math.max(1, 2)] }) }`,
      bindings: [],
    })
    expect(result.error).toBeUndefined()
    expect(result.value).toEqual({ absent: names.map(() => 'undefined'), self: true, json: '{"ok":[0,2]}' })
    const strict = await run({ program: 'undeclared = 1', bindings: [] })
    expect(strict.error).toEqual({ kind: 'exception', message: 'ReferenceError: undeclared is not defined\n    at <anonymous> (<program>:1:1)' })
  })

  it('lets a binding take a name the VM reserves for its own helpers', async () => {
    const { run } = await mountRuntime()
    const result = await run({
      program: 'return [await text.say({}), await store.put({}), typeof tools]',
      bindings: [
        { global: 'text', functions: { say: async () => 'said' } },
        { global: 'store', functions: { put: async () => 'put' } },
      ],
    })
    expect(result).toEqual({ logs: [], value: ['said', 'put', 'undefined'] })
  })

  it('starts every program in a fresh VM', async () => {
    const { run } = await mountRuntime()
    const program = 'globalThis.count = (globalThis.count ?? 0) + 1; return globalThis.count'
    expect((await run({ program, bindings: [] })).value).toBe(1)
    expect((await run({ program, bindings: [] })).value).toBe(1)
  })

  it('reports exceptions with program-relative positions and without runtime frames', async () => {
    const { run } = await mountRuntime()
    const nested = await run({ program: 'function inner() {\n  throw new Error("deep")\n}\ninner()', bindings: [] })
    expect(nested.error?.kind).toBe('exception')
    expect(nested.error?.message).toMatch(/^Error: deep\n {4}at inner \(<program>:2:\d+\)\n {4}at <anonymous> \(<program>:4:1\)$/)
    const crlf = await run({ program: 'const a = 1\r\n\r\nnull.foo', bindings: [] })
    expect(crlf.error).toEqual({ kind: 'exception', message: 'TypeError: cannot read property \'foo\' of null\n    at <anonymous> (<program>:3:1)' })
    const thrown = await run({ program: 'throw "plain"', bindings: [] })
    expect(thrown.error).toEqual({ kind: 'exception', message: 'plain' })
    const rejection = await run({ program: 'await tools.fail({})', bindings: tools({ fail: async () => { throw new Error('no') } }) })
    expect(rejection.error).toEqual({ kind: 'exception', message: 'ToolCallError: no' })
  })

  it('rejects syntax that type stripping cannot erase before starting a worker', async () => {
    const { run } = await mountRuntime()
    const result = await run({ program: 'enum Mode { A }\nreturn Mode.A', bindings: [] })
    expect(result.error?.kind).toBe('exception')
    expect(result.logs).toEqual([])
  })

  it('names close matches when a program reads a missing binding member', async () => {
    const { run } = await mountRuntime()
    const functions: Record<string, () => Promise<string>> = { read: async () => 'r', 'my-tool': async () => 'm', constructor: async () => 'c' }
    Object.defineProperty(functions, '__proto__', { value: async () => 'p', enumerable: true })
    const ok = await run({ program: 'return [await tools.read({}), await tools["my-tool"]({}), await tools.constructor({}), await tools["__proto__"]({}), "read" in tools, "nope" in tools, typeof tools.then]', bindings: tools(functions) })
    expect(ok).toEqual({ logs: [], value: ['r', 'm', 'c', 'p', true, false, 'undefined'] })
    const close = await run({ program: 'await tools.Read({})', bindings: tools(functions) })
    expect(close.error?.message)
      .toMatch(/^TypeError: tools\.Read does not exist\. Did you mean tools\.read\? Test membership with "Read" in tools\./)
    const listed = await run({ program: 'await tools.write({})', bindings: tools({ read: async () => null }) })
    expect(listed.error?.message).toMatch(/^TypeError: tools\.write does not exist\. Available: tools\.read\./)
  })

  it('runs independent binding calls concurrently', async () => {
    const { run } = await mountRuntime()
    let active = 0
    let peak = 0
    const gate = Promise.withResolvers<null>()
    const wait = async () => {
      active += 1
      peak = Math.max(peak, active)
      if (active === 3) gate.resolve(null)
      await gate.promise
      active -= 1
      return active
    }
    const result = await run({ program: 'return (await Promise.all([tools.wait({}), tools.wait({}), tools.wait({})])).length', bindings: tools({ wait }) })
    expect(result).toEqual({ logs: [], value: 3 })
    expect(peak).toBe(3)
  })

  it('fails a program that awaits a promise no binding call can settle', async () => {
    const { run } = await mountRuntime()
    const result = await run({ program: 'console.log("before"); await new Promise(() => {})', bindings: [] })
    expect(result.logs).toEqual(['before'])
    expect(result.error?.kind).toBe('exception')
    expect(result.error?.message).toContain('can never settle')
  })

  it('reports a VM allocation beyond the memory limit as a program exception', async () => {
    const { run } = await mountRuntime({ maxMemoryBytes: 16 * 1024 * 1024 })
    const result = await run({ program: 'const parts = []; for (;;) parts.push("x".repeat(1 << 20) + parts.length)', bindings: [] })
    expect(result.error?.kind).toBe('exception')
    expect(result.error?.message).toContain('out of memory')
  })
})

describe('lossless JSON at the binding and completion boundaries', () => {
  it('rejects lossy arguments inside the program without calling the binding', async () => {
    const { run } = await mountRuntime()
    let calls = 0
    const echo = async (args: unknown) => { calls += 1; return args as null }
    for (const args of ['{ x: undefined }', '-0', 'NaN', '[1, , 2]', 'new Map()', '(() => { const a = []; a.push(a); return a })()', 'Object.assign([1], { extra: true })', '{ [Symbol("s")]: 1 }']) {
      const result = await run({ program: `try { await tools.echo(${args}) } catch (error) { return [error instanceof ToolCallError, error.toolName, error.message] }`, bindings: tools({ echo }) })
      expect(result.value, args).toEqual([true, 'echo', 'binding arguments must be lossless JSON'])
    }
    expect(calls).toBe(0)
    const accepted = await run({ program: 'return await tools.echo({ a: [1, "two", null, true, { b: 1.5 }], shared: [Object.create(null)] })', bindings: tools({ echo }) })
    expect(accepted.value).toEqual({ a: [1, 'two', null, true, { b: 1.5 }], shared: [{}] })
  })

  it('rejects a lossy binding resolution as that call failing', async () => {
    const { run } = await mountRuntime()
    const result = await run({ program: 'try { await tools.bad({}) } catch (error) { return error.message }', bindings: tools({ bad: async () => ({ x: undefined }) as never }) })
    expect(result.value).toBe('binding resolution must be lossless JSON')
  })

  it('reports a lossy completion as invalid output', async () => {
    const { run } = await mountRuntime()
    for (const value of ['-0', 'Infinity', '{ f: () => 1 }', 'new Date(0)', '[undefined]']) {
      expect((await run({ program: `return ${value}`, bindings: [] })).error, value).toEqual({ kind: 'invalid-output', message: 'program completion must be lossless JSON' })
    }
    expect(await run({ program: 'return', bindings: [] })).toEqual({ logs: [] })
    expect(await run({ program: 'return null', bindings: [] })).toEqual({ logs: [], value: null })
  })

  it('treats direct use of the call channel as a protocol failure', async () => {
    const { run } = await mountRuntime()
    const call = (args: string) => `await globalThis.__dsh_call__(${args})`
    expect((await run({ program: call('"bad"'), bindings: tools({ ok: async () => null }) })).error).toEqual({ kind: 'protocol', message: 'invalid binding call' })
    expect((await run({ program: call('[0, "missing", {}]'), bindings: tools({ ok: async () => null }) })).error).toEqual({ kind: 'protocol', message: 'program requested an undeclared binding' })
    expect((await run({ program: call('[3, "ok", {}]'), bindings: tools({ ok: async () => null }) })).error).toEqual({ kind: 'protocol', message: 'program requested an undeclared binding' })
    expect((await run({ program: call('[0, "toString", {}]'), bindings: tools({ ok: async () => null }) })).error).toEqual({ kind: 'protocol', message: 'program requested an undeclared binding' })
    expect(await run({ program: `return ${call('[0, "ok", { a: 1 }]')}`, bindings: tools({ ok: async args => args as null }) })).toEqual({ logs: [], value: { a: 1 } })
  })
})

describe('limits', () => {
  it('stops a program whose console output crosses the budget and keeps the fitting prefix', async () => {
    const { run } = await mountRuntime({ maxOutputBytes: 100 })
    const result = await run({ program: 'for (;;) console.log("a".repeat(40))', bindings: [] })
    expect(result.error?.kind).toBe('output-limit')
    expect(result.logs[0]).toBe('a'.repeat(40))
    expect(outerBytes(result)).toBeLessThanOrEqual(100)
  })

  it('applies the budget to completion values and failure diagnostics', async () => {
    const { run } = await mountRuntime({ maxOutputBytes: 64 })
    expect(await run({ program: 'return "x".repeat(60)', bindings: [] })).toEqual({ logs: [], value: 'x'.repeat(60) })
    const value = await run({ program: 'return "x".repeat(61)', bindings: [] })
    expect(value.error?.kind).toBe('output-limit')
    expect(outerBytes(value)).toBeLessThanOrEqual(64)
    const logged = await run({ program: 'console.log("y".repeat(20)); return "x".repeat(40)', bindings: [] })
    expect(logged.error?.kind).toBe('output-limit')
    const diagnostic = await run({ program: 'throw new Error("z".repeat(100))', bindings: [] })
    expect(diagnostic.error?.kind).toBe('output-limit')
    expect(outerBytes(diagnostic)).toBeLessThanOrEqual(64)
  })

  it('accounts multibyte and escaped output exactly at the limit', async () => {
    const text = '"€\n😀'
    const exact = Buffer.byteLength(JSON.stringify([text]))
    const fits = await mountRuntime({ maxOutputBytes: exact })
    expect(await fits.run({ program: `console.log(${JSON.stringify(text)})`, bindings: [] })).toEqual({ logs: [text] })
    const over = await mountRuntime({ maxOutputBytes: exact - 1 })
    expect((await over.run({ program: `console.log(${JSON.stringify(text)})`, bindings: [] })).error?.kind).toBe('output-limit')
  })

  it('fails a program whose outstanding binding calls exceed the configured limits', async () => {
    const { run } = await mountRuntime({ maxPendingCalls: 2, maxMessageBytes: 1000 })
    const release = Promise.withResolvers<null>()
    const wait = async () => await release.promise
    const calls = await run({ program: 'await Promise.all([tools.wait({}), tools.wait({}), tools.wait({})])', bindings: tools({ wait }) })
    expect(calls.error).toEqual({ kind: 'protocol', message: 'pending binding calls exceed configured limits' })
    release.resolve(null)
    const echo = async (args: unknown) => args as null
    expect((await run({ program: 'return (await tools.echo("x".repeat(900))).length', bindings: tools({ echo }) })).value).toBe(900)
    for (const size of [1500, 5000]) {
      const bytes = await run({ program: `await tools.echo("x".repeat(${size}))`, bindings: tools({ echo }) })
      expect(bytes.error, String(size)).toEqual({ kind: 'protocol', message: 'pending binding calls exceed configured limits' })
    }
  })

  it('times out a busy program and one waiting on a binding', async () => {
    const { run } = await mountRuntime()
    const busy = await run({ program: 'console.log("start"); for (;;) {}', bindings: [], timeoutMs: 300 })
    expect(busy).toEqual({ logs: ['start'], error: { kind: 'timeout', message: 'execution deadline reached (300ms)' } })
    const waiting = await run({ program: 'await tools.wait({})', bindings: tools({ wait: () => new Promise(() => {}) }), timeoutMs: 300 })
    expect(waiting.error?.kind).toBe('timeout')
  })
})

describe('cancellation and disposal', () => {
  it('aborts a live program with the caller reason', async () => {
    const { run } = await mountRuntime()
    const entered = Promise.withResolvers<null>()
    const controller = new AbortController()
    const active = run({ program: 'void tools.enter({}); for (;;) {}', signal: controller.signal, bindings: tools({ enter: async () => { entered.resolve(null); return null } }) })
    await entered.promise
    controller.abort('stop')
    expect((await active).error).toEqual({ kind: 'abort', message: 'stop' })
  })

  it('does not start a program whose signal is already aborted', async () => {
    const { run } = await mountRuntime()
    let called = false
    const result = await run({ program: 'await tools.mark({})', signal: AbortSignal.abort('early'), bindings: tools({ mark: async () => { called = true; return null } }) })
    expect(result.error).toEqual({ kind: 'abort', message: 'early' })
    expect(called).toBe(false)
  })

  it('aborts and awaits live programs on disposal, then refuses new work and unregisters', async () => {
    const { ctx, runtime, run } = await mountRuntime()
    const spec = runtime.resolve({ program: '', bindings: [] })
    const entered = Promise.withResolvers<null>()
    const active = run({ program: 'await tools.enter({}); await tools.hang({})', bindings: tools({ enter: async () => { entered.resolve(null); return null }, hang: () => new Promise(() => {}) }) })
    await entered.promise
    await ctx.fiber.dispose()
    expect((await active).error).toEqual({ kind: 'abort', message: 'runtime disposed' })
    await expect(runtime.run(spec)).rejects.toThrow('disposal')
    expect(() => runtime.resolve({ program: '', bindings: [] })).toThrow('disposal')
    expect(ctx.get('ptcRuntime')).toBeUndefined()
  })
})

describe('resolution and configuration', () => {
  it('resolves directory and deadline choices', async () => {
    const { runtime } = await mountRuntime({ timeoutMs: 1000, maxTimeoutMs: 5000 })
    expect(runtime.language).toBe('typescript')
    expect(runtime.isolation).toBe('worker-thread')
    expect(runtime.sandboxMode).toBeUndefined()
    expect(runtime.timeout).toEqual({ defaultMs: 1000, maxMs: 5000 })
    expect(runtime.executionInstructions).toContain('fresh JavaScript sandbox')
    expect(runtime.resolve({ program: '', bindings: [] })).toMatchObject({ cwd: process.cwd(), timeoutMs: 1000 })
    expect(runtime.resolve({ program: '', bindings: [], timeoutMs: 60_000, cwd: '/work' })).toMatchObject({ cwd: '/work', timeoutMs: 5000 })
    expect(() => runtime.resolve({ program: '', bindings: [], timeoutMs: 0 })).toThrow('timeoutMs')
    expect(() => runtime.resolve({ program: '', bindings: [], cwd: 'relative' })).toThrow('absolute')
    expect(() => runtime.resolve({ program: '', bindings: [], sandboxPolicy: { mode: 'read-only', workspaceRoot: '/' } as never })).toThrow('sandbox policy is unsupported')
    await expect(runtime.run({ program: '', bindings: [], cwd: '/', timeoutMs: 10_000 })).rejects.toThrow('resolved cwd and timeout')
  })

  it.each([
    [{ timeoutMs: 0 }, 'positive'],
    [{ maxOutputBytes: 3 }, 'at least 4'],
    [{ maxPendingCalls: 1.5 }, 'integer'],
    [{ maxTimeoutMs: 2 ** 32 }, 'timer range'],
  ])('rejects configuration %j', async (config, message) => {
    await expect(mountRuntime(config)).rejects.toThrow(message)
  })

  it.each([
    [[{ global: 'console', functions: {} }], 'reserved binding global'],
    [[{ global: '$tools', functions: {} }], 'not a usable identifier'],
    [[{ global: 'tools', functions: {} }, { global: 'tools', functions: {} }], 'duplicate binding global'],
    [[{ global: 'tools', functions: {}, errorClass: { name: 'tools', memberNameProperty: 'toolName' } }], 'duplicate injected global'],
    [[{ global: 'tools', functions: {}, errorClass: { name: 'ToolCallError', memberNameProperty: 'message' } }], 'is not usable'],
  ])('rejects unusable bindings %#', async (bindings, message) => {
    const { run } = await mountRuntime()
    await expect(run({ program: '', bindings })).rejects.toThrow(message)
  })
})
