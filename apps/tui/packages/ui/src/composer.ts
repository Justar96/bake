/** Cursor editing and reversible history browsing for Ink's separate input channels. */
import { useEffect, useRef, useState } from 'react'
import type { Key } from 'ink'
import { composerText, draftAt, eraseAtCursor, insertText, moveCursor, moveVertically, type Draft } from './editor.ts'
import { recallCursor } from './history.ts'

interface HistoryVisit {
  readonly iterator: Iterator<string>
  readonly scratch: Draft
  readonly entries: Draft[]
  index: number
}

/**
 * Acceptance is synchronous for ordinary input and asynchronous for attachment admission.
 * @param text - the complete composer draft.
 * @returns false to keep the draft, or a promise that blocks editing until acceptance.
 */
export type Submit = (text: string) => void | boolean | Promise<void | boolean>

/**
 * Whether a key inserts a line break instead of submitting.
 *
 * Most terminals send Shift-Enter as the same carriage return as Enter, so
 * Shift-Enter breaks a line only where the terminal reports it as CSI-u. Two
 * keys break one everywhere. Ctrl-J sends a line feed, which arrives as a read
 * of its own. Alt-Enter, or Option-Enter where Option acts as Meta, sends
 * Escape and a carriage return, which Ink decodes as a Meta Return. A terminal
 * that reports Ctrl-J as CSI-u sends its letter with Ctrl instead. A line feed
 * that ends other text in the same read is typing followed by Enter, so it
 * still submits.
 *
 * @param text - the text Ink decoded from one read.
 * @param key - the key Ink decoded from it.
 * @returns true for Shift-, Alt-, or Meta-Enter, and a read of line feeds or Ctrl-J alone.
 */
export function isNewline(text: string, key: Key): boolean {
  if (key.return) return key.shift || key.meta
  return /^\n+$/u.test(text) || (key.ctrl && !key.meta && text === 'j')
}

/**
 * Keep edits from the same input read available before React paints the next frame.
 *
 * @param submit - explicit Enter action. False or rejection keeps the complete draft and cursor.
 * @param history - optional lazy session input, newest first. Omitted for secrets and questions.
 * @param allowEmpty - permit Enter with only staged attachments, or on an empty field that has a default.
 * @param initial - text the draft opens with, the caret after it; read on mount only.
 * @returns rendered text and cursor, synchronous values, editing actions, and history navigation.
 */
export function useComposer(submit: Submit, history?: () => Iterable<string>, allowEmpty = false, initial = '') {
  const [draft, setDraft] = useState(() => draftAt(composerText(initial)))
  const [submitting, setSubmitting] = useState(false)
  const pending = useRef(false)
  const mounted = useRef(true)
  useEffect(() => { mounted.current = true; return () => { mounted.current = false } }, [])
  const current = useRef(draft)
  const visit = useRef<HistoryVisit | undefined>(undefined)
  // The column a run of Up and Down presses keeps to. Any other change ends the run.
  const goal = useRef<number | undefined>(undefined)
  const show = (value: Draft): void => { current.current = value; setDraft(value) }
  const update = (value: Draft): void => { goal.current = undefined; show(value) }
  return {
    submitting,
    get blocked(): boolean { return pending.current },
    text: draft.text, cursor: draft.cursor,
    before: draft.text.slice(0, draft.cursor), after: draft.text.slice(draft.cursor),
    get value(): string { return current.current.text },
    get position(): number { return current.current.cursor },
    replace: (value: string, cursor?: number): void => { if (!pending.current) update(draftAt(composerText(value), cursor)) },
    paste: (value: string): void => { if (!pending.current) update(insertText(current.current, value)) },
    erase: (): void => { if (!pending.current) update(eraseAtCursor(current.current, 'backward')) },
    editKey: (text: string, key: Key): boolean => {
      if (pending.current) return true
      if (key.meta) return false
      const direction = key.leftArrow ? 'left' : key.rightArrow ? 'right'
        : key.home || (key.ctrl && text === 'a') ? 'home' : key.end || (key.ctrl && text === 'e') ? 'end' : undefined
      if (direction !== undefined) update(moveCursor(current.current, direction))
      else if (key.backspace || key.delete) update(eraseAtCursor(current.current, key.backspace ? 'backward' : 'forward'))
      else return false
      return true
    },
    /** Stop browsing history and restore the draft the browse started from. */
    leave: (): void => {
      if (visit.current === undefined) return
      update(visit.current.scratch)
      visit.current = undefined
    },
    /**
     * Move the caret to the screen row above or below, keeping its column over a run of presses.
     * @param direction - the row to move to.
     * @param width - columns the composer wraps each row at.
     * @returns false on the first row (up) or the last row (down), where recall takes over.
     */
    vertical: (direction: 'up' | 'down', width: number): boolean => {
      if (pending.current) return false
      const moved = moveVertically(current.current, width, direction, goal.current)
      if (moved === undefined) return false
      show(moved.draft)
      goal.current = moved.goal
      return true
    },
    /**
     * Step through input history.
     * @param direction - older for Up, newer for Down.
     * @param rows - screen rows an entry occupies. Given, an entry of several
     *   rows opens with the caret on the row the next press leaves from.
     * @returns false when there was nothing further to show.
     */
    recall: (direction: 'older' | 'newer', rows?: (text: string) => number): boolean => {
      if (pending.current || history === undefined) return false
      if (visit.current === undefined) {
        if (direction === 'newer') return false
        visit.current = { iterator: history()[Symbol.iterator](), scratch: current.current, entries: [], index: -1 }
      }
      const active = visit.current
      if (active.index >= 0) active.entries[active.index] = current.current
      const next = active.index + (direction === 'older' ? 1 : -1)
      if (next < 0) { update(active.scratch); visit.current = undefined; return true }
      if (next === active.entries.length) {
        const item = active.iterator.next()
        if (item.done === true) { if (active.entries.length === 0) visit.current = undefined; return false }
        active.entries.push(draftAt(composerText(item.value)))
      }
      active.index = next
      const entry = active.entries[next]!
      update(rows === undefined ? entry : draftAt(entry.text, recallCursor(entry, direction, rows(entry.text))))
      return true
    },
    type: (value: string): void => {
      if (pending.current) return
      const lines = composerText(value).split('\n')
      let edited = insertText(current.current, lines[0]!)
      for (const line of lines.slice(1)) {
        if (edited.text.trim() !== '' || allowEmpty) {
          const accepted = submit(edited.text)
          if (accepted === false) { update(edited); return }
          if (accepted instanceof Promise) {
            pending.current = true
            setSubmitting(true)
            update(edited)
            const settle = (ok: boolean | void): void => {
              if (!mounted.current) return
              pending.current = false
              setSubmitting(false)
              if (ok !== false) { visit.current = undefined; update(draftAt('')) }
            }
            void accepted.then(settle, () => settle(false))
            return
          }
        }
        visit.current = undefined
        edited = draftAt(line)
      }
      update(edited)
    },
  }
}
