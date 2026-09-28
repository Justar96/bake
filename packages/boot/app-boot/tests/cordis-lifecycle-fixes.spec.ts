/**
 * Regression coverage for vendored Cordis/Loader/Timer lifecycle fixes,
 * exercised through a real Loader-composed tree rather than unit-level mocks:
 * the `internal/update` listener registration leaking across a reload,
 * `DisposableList.unshift` (used by that same listener path), the `parallel`
 * dispatch mode reported on `internal/dispatch`, a disposed `ctx.throttle`
 * still firing its leading edge, and Loader isolate-realm garbage collection
 * stopping early. See vendor/README.md "Local modifications" for the fixes.
 */

import { describe, expect, it, onTestFinished } from 'vitest'
import { Context, type Fiber } from '@deepseek-ai/cordis'
import Loader from '@deepseek-ai/cordis-plugin-loader'
import Timer from '@deepseek-ai/cordis-plugin-timer'

declare module '@deepseek-ai/cordis' {
  interface Events {
    'lifecycle-fix-test/event'(): void
  }
}

function context(): Context {
  const ctx = new Context()
  onTestFinished(() => ctx.fiber.dispose())
  return ctx
}

async function runtimeContext(): Promise<Context> {
  const ctx = context()
  await ctx.plugin(Loader)
  return ctx
}

async function mountEntry(ctx: Context, plugin: unknown, config: unknown = {}): Promise<Fiber> {
  const name = `consumer-${Object.keys(ctx.loader.builtins).length}`
  ctx.loader.builtins[name] = plugin
  const id = await ctx.loader.create({ name: `cordis:${name}`, config })
  const fiber = ctx.loader.resolve(id).fiber!
  await fiber.await()
  return fiber
}

describe('EventsService internal/update listener lifecycle', () => {
  it('disposes a fiber-owned internal/update listener on reload instead of leaking it', async () => {
    const ctx = await runtimeContext()
    const calls: number[] = []
    let mounts = 0
    const fiber = await mountEntry(ctx, {
      apply(owner: Context) {
        const generation = ++mounts
        owner.on('internal/update', (_config: unknown, _noSave: boolean, next: () => unknown) => {
          calls.push(generation)
          return next()
        })
      },
    }, { value: 1 })

    // `Fiber.update()` does not return or await its own restart, so poll
    // `fiber.await()` between calls instead of trusting `update()`'s result.
    fiber.update({ value: 2 })
    await fiber.await()
    // Only the listener registered by the current (first) load should fire.
    expect(calls).toEqual([1])

    calls.length = 0
    fiber.update({ value: 3 })
    await fiber.await()
    // A leaked listener from generation 1 or 2 would show up here too; the
    // fix ties the registration to the fiber's own effect list, so reload
    // disposes it before the plugin body re-registers a fresh one.
    expect(calls).toEqual([2])
  })

  it('prepends an internal/update listener without throwing (DisposableList.unshift)', async () => {
    const ctx = await runtimeContext()
    const order: string[] = []
    const fiber = await mountEntry(ctx, {
      apply(owner: Context) {
        owner.on('internal/update', (_config: unknown, _noSave: boolean, next: () => unknown) => {
          order.push('normal')
          return next()
        })
        owner.on('internal/update', (_config: unknown, _noSave: boolean, next: () => unknown) => {
          order.push('prepended')
          return next()
        }, { prepend: true })
      },
    }, { value: 1 })

    fiber.update({ value: 2 })
    await fiber.await()
    expect(order).toEqual(['prepended', 'normal'])
  })
})

describe('EventsService.parallel dispatch mode', () => {
  it('reports "parallel" (not "emit") on internal/dispatch', async () => {
    const ctx = context()
    const modes: string[] = []
    ctx.on('internal/dispatch', (mode) => { modes.push(mode) })
    await ctx.parallel('lifecycle-fix-test/event')
    expect(modes).toEqual(['parallel'])
  })
})

describe('ctx.throttle after dispose', () => {
  it('is a complete no-op once the owning fiber is disposed (no leading-edge call)', async () => {
    const ctx = await runtimeContext()
    await ctx.plugin(Timer)
    const calls: number[] = []
    let throttled!: (n: number) => void
    const fiber = await mountEntry(ctx, {
      inject: ['timer'],
      apply(owner: Context) {
        throttled = owner.throttle((n: number) => calls.push(n), 1000)
      },
    })
    await fiber.dispose()
    throttled(1)
    expect(calls).toEqual([])
  })
})

describe('Loader isolate realm cleanup', () => {
  it('continues checking every isolated name after removal instead of stopping at the first still-referenced one', async () => {
    const ctx = await runtimeContext()
    ctx.loader.builtins.noop = { apply() {} }

    // `survivor` keeps the 'realm-a' realm referenced through the 'first'
    // name so the cleanup scan for 'first' finds a reference and must move on
    // to 'second' instead of stopping there.
    await ctx.loader.create({ name: 'cordis:noop', isolate: { first: 'realm-a' } })
    const leavingId = await ctx.loader.create({
      name: 'cordis:noop',
      isolate: { first: 'realm-a', second: 'realm-b' },
    })
    const leaving = ctx.loader.resolve(leavingId)
    const secondSymbolBefore = (leaving.ctx as unknown as Record<symbol, Record<string, symbol>>)[Context.isolate].second

    ctx.loader.remove(leavingId)
    await ctx.loader.await()

    const laterId = await ctx.loader.create({ name: 'cordis:noop', isolate: { second: 'realm-b' } })
    const later = ctx.loader.resolve(laterId)
    const secondSymbolAfter = (later.ctx as unknown as Record<symbol, Record<string, symbol>>)[Context.isolate].second

    // 'realm-b' (checked second) had no other referencer once `leavingId` was
    // removed, so it should be garbage collected and get a fresh symbol here.
    // A `return` instead of `continue` after finding 'realm-a' still
    // referenced would skip 'second' entirely, leaking the old symbol.
    expect(secondSymbolAfter).not.toBe(secondSymbolBefore)
  })
})
