/** Cursor editing and reversible history browsing for Ink's separate input channels. */
import { useEffect, useRef, useState } from 'react'
import type { Key } from 'ink'
import { composerText, draftAt, eraseAtCursor, insertText, moveCursor, type Draft } from './editor.ts'

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
 * Keep edits from the same input read available before React paints the next frame.
 *
 * @param submit - explicit Enter action. False or rejection keeps the complete draft and cursor.
 * @param history - optional lazy session input, newest first. Omitted for secrets and questions.
 * @param allowEmpty - permit Enter with only staged attachments.
 * @returns rendered text and cursor, synchronous values, editing actions, and history navigation.
 */
export function useComposer(submit: Submit, history?: () => Iterable<string>, allowEmpty = false) {
  const [draft, setDraft] = useState(() => draftAt(''))
  const [submitting, setSubmitting] = useState(false)
  const pending = useRef(false)
  const mounted = useRef(true)
  useEffect(() => { mounted.current = true; return () => { mounted.current = false } }, [])
  const current = useRef(draft)
  const visit = useRef<HistoryVisit | undefined>(undefined)
  const update = (value: Draft): void => { current.current = value; setDraft(value) }
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
    recall: (direction: 'older' | 'newer'): void => {
      if (pending.current || history === undefined) return
      if (visit.current === undefined) {
        if (direction === 'newer') return
        visit.current = { iterator: history()[Symbol.iterator](), scratch: current.current, entries: [], index: -1 }
      }
      const active = visit.current
      if (active.index >= 0) active.entries[active.index] = current.current
      const next = active.index + (direction === 'older' ? 1 : -1)
      if (next < 0) { update(active.scratch); visit.current = undefined; return }
      if (next === active.entries.length) {
        const item = active.iterator.next()
        if (item.done === true) { if (active.entries.length === 0) visit.current = undefined; return }
        active.entries.push(draftAt(composerText(item.value)))
      }
      active.index = next
      update(active.entries[next]!)
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
