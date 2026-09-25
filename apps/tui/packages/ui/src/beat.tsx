/**
 * One animation beat for the whole surface.
 *
 * Every moving part — the turn header's spinner, a running action's
 * blinking marker — takes its time from here instead of keeping a timer of
 * its own. Separate timers fire at unrelated moments, and each firing is a
 * commit that lays out the whole live surface again, so two running actions
 * and a header cost three layouts where one would do. Here there is one
 * timer, and it commits only when a frame would draw something different.
 * Each part says what it would show at a time as a short key, and a beat on
 * which no key changed renders nothing. While nothing subscribes — no turn,
 * no running action, a header with no room to draw — there is no timer.
 *
 * The provider holds the time in state, so a beat that changes something is
 * one commit that re-renders only the parts reading it. Its children are the
 * same element on every beat, and React skips them.
 *
 * @module @dsh-tui/ui/beat
 */

import React, { createContext, useContext, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react'
import { FRAME_MS, type Clock } from './activity.ts'

/** What a moving part would draw at a time, as a key that differs when the drawing does. */
export type View = (now: number) => string

/** A subscription's handle. Resynchronise its key after a render, or end it. */
interface Subscription {
  /** Record the key the part just drew, so the next beat compares against the screen. */
  readonly drawn: (key: string) => void
  readonly dispose: () => void
}

interface BeatValue {
  /** The time the surface is drawn at. */
  readonly now: number
  readonly subscribe: (view: View, drawn: string) => Subscription
}

const BeatContext = createContext<BeatValue | undefined>(undefined)

/**
 * The timer behind a provider. One `clock.every` while any part subscribes.
 *
 * @param clock - the time source.
 * @param publish - commits a new time; called at most once a beat.
 * @returns how parts subscribe, and a stop for the provider's teardown.
 */
export function beat(clock: Clock, publish: (now: number) => void): {
  readonly subscribe: (view: View, drawn: string) => Subscription
  readonly stop: () => void
} {
  const parts = new Set<{ readonly view: View, key: string }>()
  let stop: (() => void) | undefined
  const tick = (): void => {
    const now = clock.now()
    let changed = false
    for (const part of parts) {
      const key = part.view(now)
      if (key !== part.key) { part.key = key; changed = true }
    }
    if (changed) publish(now)
  }
  const halt = (): void => { stop?.(); stop = undefined }
  return {
    subscribe(view, drawn) {
      const part = { view, key: drawn }
      parts.add(part)
      stop ??= clock.every(FRAME_MS, tick)
      return {
        drawn: key => { part.key = key },
        dispose: () => {
          parts.delete(part)
          if (parts.size === 0) halt()
        },
      }
    },
    stop() {
      parts.clear()
      halt()
    },
  }
}

/**
 * Provide the beat to the parts below.
 *
 * @param props.clock - time source; absent, nothing below moves.
 * @param props.children - the surface.
 */
export function Beat({ clock, children }: { readonly clock: Clock | undefined, readonly children: React.ReactNode }): React.ReactElement {
  const [now, setNow] = useState(() => clock?.now() ?? 0)
  const timer = useMemo(() => clock === undefined ? undefined : beat(clock, setNow), [clock])
  // Parts dispose their own subscriptions first; this catches a clock change,
  // which replaces the timer while its parts resubscribe to the new one.
  useEffect(() => timer?.stop, [timer])
  const value = useMemo(() => timer === undefined ? undefined : { now, subscribe: timer.subscribe }, [now, timer])
  return <BeatContext.Provider value={value}>{children}</BeatContext.Provider>
}

/**
 * The time a moving part draws at, advancing on the beats where its view changes.
 *
 * @param enabled - whether it moves now; off, it holds no subscription and the
 *   surface keeps no timer for it.
 * @param view - what it would draw at a time, as a key.
 * @returns the time to draw at, or undefined with no beat above or no clock.
 */
export function useBeat(enabled: boolean, view: View): number | undefined {
  const value = useContext(BeatContext)
  const latest = useRef(view)
  latest.current = view
  const subscription = useRef<Subscription | undefined>(undefined)
  const now = value?.now
  const subscribe = enabled ? value?.subscribe : undefined
  useEffect(() => {
    if (subscribe === undefined) return undefined
    const current = subscribe(time => latest.current(time), latest.current(now ?? 0))
    subscription.current = current
    return () => {
      current.dispose()
      subscription.current = undefined
    }
    // `now` is read once, for the key already drawn. Later renders
    // resynchronise below instead of resubscribing on every beat.
  }, [subscribe])
  // A render for any reason — a new beat, or new props that change the view —
  // is what is on screen now, so the next beat compares against it.
  useLayoutEffect(() => {
    if (now !== undefined) subscription.current?.drawn(view(now))
  })
  return now
}
