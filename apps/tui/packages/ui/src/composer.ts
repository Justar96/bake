/** Cursor editing and reversible history browsing for Ink's separate input channels. */
import { useEffect, useRef, useState } from 'react'
import type { Key } from 'ink'
import {
  atomRange, composerText, draftAt, eraseAtCursor, insertText, moveCursor, moveToRowEdge, moveVertically, outsideAtoms,
  wordStop, type Draft,
} from './editor.ts'
import { recallCursor } from './history.ts'
import { collapses, expandPastes, imageToken, pastedTextToken, type PasteAtom } from './paste.ts'

/** Undo steps a draft keeps. */
const UNDO_LIMIT = 100
/** Killed texts the yank ring keeps. */
const RING_LIMIT = 30
/** What Ctrl+-, Ctrl+_, and Ctrl+/ send: the unit separator, which Ink passes on as text. */
const UNDO_KEY = '\u001f'

/**
 * The last edit, which decides what the next one joins: typing within a word
 * is one undo step, consecutive kills are one yank, and only a yank can be
 * replaced by an older kill.
 */
type Action = 'type' | 'kill' | 'yank' | undefined

/** Killed text, and the pasted text its placeholders stand for. */
interface Killed {
  readonly text: string
  readonly atoms: ReadonlyMap<string, PasteAtom>
}

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
 * @param removeImage - told the attachment key of an image placeholder that
 *   left the draft, by an erase, a kill, or an undo. Undo and yank never bring
 *   it back.
 * @param options.opaque - edit each line as one word, so word steps over a
 *   masked secret reveal nothing about its characters.
 * @returns rendered text and cursor, synchronous values, editing actions, and history navigation.
 */
export function useComposer(submit: Submit, history?: () => Iterable<string>, allowEmpty = false, initial = '',
  removeImage?: (key: string) => void, options: { readonly opaque?: boolean } = {}) {
  const [draft, setDraft] = useState(() => draftAt(composerText(initial)))
  const [submitting, setSubmitting] = useState(false)
  const pending = useRef(false)
  const mounted = useRef(true)
  useEffect(() => { mounted.current = true; return () => { mounted.current = false } }, [])
  const current = useRef(draft)
  const visit = useRef<HistoryVisit | undefined>(undefined)
  // The column a run of Up and Down presses keeps to. Any other change ends the run.
  const goal = useRef<number | undefined>(undefined)
  // Placeholders this draft inserted, and the number the next one takes. A
  // text placeholder stays registered after it is erased, so undo and yank
  // can bring it back; an image's attachment is gone once its placeholder is.
  const atoms = useRef(new Map<string, PasteAtom>())
  const pastes = useRef(0)
  const unstaged = useRef(new Set<string>())
  const last = useRef<Action>(undefined)
  const undo = useRef<Draft[]>([])
  // Killed text, newest last, and where the last yank put the newest.
  const ring = useRef<Killed[]>([])
  const yanked = useRef<{ readonly start: number, readonly end: number } | undefined>(undefined)
  const opaque = options.opaque === true
  const keys = (): string[] => [...atoms.current.keys()]
  const show = (value: Draft): void => { current.current = value; setDraft(value) }
  const update = (value: Draft): void => { goal.current = undefined; show(value) }
  const forget = (): void => {
    atoms.current.clear(); unstaged.current.clear(); pastes.current = 0
    undo.current = []; last.current = undefined
  }
  /** Keep the draft before an edit, once per distinct state. */
  const remember = (): void => {
    const top = undo.current.at(-1)
    if (top?.text === current.current.text && top.cursor === current.current.cursor) return
    undo.current.push(current.current)
    if (undo.current.length > UNDO_LIMIT) undo.current.shift()
  }
  /** Apply an edit, unstaging each image whose placeholder it removed. */
  const commit = (next: Draft, action: Action): void => {
    for (const [token, atom] of atoms.current) {
      if (atom.kind !== 'image' || !current.current.text.includes(token) || next.text.includes(token)) continue
      atoms.current.delete(token)
      unstaged.current.add(token)
      removeImage?.(atom.key)
    }
    last.current = action
    update(next)
  }
  /** A caret move, which ends a run of typing, kills, or yanks. */
  const move = (cursor: number): void => {
    last.current = undefined
    update(draftAt(current.current.text, cursor))
  }
  /** Remove image placeholders whose attachment was unstaged from restored text. */
  const restorable = (draft: Draft): Draft => {
    let { text, cursor } = draft
    for (const token of unstaged.current) {
      for (let at = text.lastIndexOf(token); at !== -1; at = text.lastIndexOf(token, at - 1)) {
        text = text.slice(0, at) + text.slice(at + token.length)
        if (cursor > at) cursor = Math.max(at, cursor - token.length)
        if (at === 0) break
      }
    }
    return draftAt(text, cursor)
  }
  const erase = (direction: 'backward' | 'forward'): void => {
    const next = eraseAtCursor(current.current, direction, keys())
    if (next.text === current.current.text) return
    remember()
    commit(next, undefined)
  }
  /**
   * Cut a range into the yank ring. Consecutive kills join one entry, in the
   * order the text stood.
   */
  const kill = (from: number, to: number, backward: boolean): void => {
    const { text } = current.current
    const { start, end } = atomRange(text, from, to, keys())
    if (start === end) return
    remember()
    let removed = text.slice(start, end)
    const pasted = new Map<string, PasteAtom>()
    for (const [token, atom] of atoms.current) {
      // An image placeholder's attachment goes with it; a yank brings back only text.
      if (atom.kind === 'image') removed = removed.split(token).join('')
      else if (removed.includes(token)) pasted.set(token, atom)
    }
    const entries = ring.current
    const joined = last.current === 'kill' ? entries.pop() : undefined
    entries.push(joined === undefined ? { text: removed, atoms: pasted } : {
      text: backward ? removed + joined.text : joined.text + removed, atoms: new Map([...joined.atoms, ...pasted]),
    })
    if (entries.length > RING_LIMIT) entries.shift()
    commit(draftAt(text.slice(0, start) + text.slice(end), start), 'kill')
  }
  const killWord = (direction: 'left' | 'right'): void => {
    const { text, cursor } = current.current
    const stop = wordStop(text, cursor, direction, keys(), opaque)
    kill(Math.min(stop, cursor), Math.max(stop, cursor), direction === 'left')
  }
  /** Ctrl+U and Ctrl+K: to the logical line's edge, or across the line break at it. */
  const killLine = (direction: 'left' | 'right'): void => {
    const { text, cursor } = current.current
    if (direction === 'left') {
      const start = text.lastIndexOf('\n', cursor - 1) + 1
      kill(start === cursor ? Math.max(0, cursor - 1) : start, cursor, true)
    } else {
      const next = text.indexOf('\n', cursor)
      const end = next < 0 ? text.length : next
      kill(cursor, end === cursor ? Math.min(text.length, cursor + 1) : end, false)
    }
  }
  /**
   * The text a yank inserts. A pasted-text placeholder this draft no longer
   * registers, as after a submission, takes a fresh number, so it can never
   * expand to another paste's text.
   */
  const yankable = (entry: Killed): string => {
    let text = entry.text
    for (const [token, atom] of entry.atoms) {
      if (atoms.current.get(token) === atom || atom.kind !== 'text') continue
      const fresh = pastedTextToken(++pastes.current, atom.text)
      atoms.current.set(fresh, atom)
      text = text.split(token).join(fresh)
    }
    return text
  }
  const yank = (): void => {
    const entry = ring.current.at(-1)
    if (entry === undefined || entry.text === '') return
    remember()
    const inserted = yankable(entry)
    const { text, cursor } = current.current
    yanked.current = { start: cursor, end: cursor + inserted.length }
    commit(draftAt(text.slice(0, cursor) + inserted + text.slice(cursor), cursor + inserted.length), 'yank')
  }
  /** Alt+Y right after a yank: replace what it put in with the next older kill. */
  const yankPop = (): void => {
    const range = yanked.current
    if (last.current !== 'yank' || range === undefined || ring.current.length < 2) return
    remember()
    ring.current.unshift(ring.current.pop()!)
    const inserted = yankable(ring.current.at(-1)!)
    const { text } = current.current
    yanked.current = { start: range.start, end: range.start + inserted.length }
    commit(draftAt(text.slice(0, range.start) + inserted + text.slice(range.end), range.start + inserted.length), 'yank')
  }
  /** Return to the draft before the last edit. A history browse ends with it. */
  const undoLast = (): void => {
    const previous = undo.current.pop()
    if (previous === undefined) return
    visit.current = undefined
    commit(restorable(previous), undefined)
  }
  const accept = (text: string) => submit(expandPastes(text, atoms.current))
  return {
    submitting,
    get blocked(): boolean { return pending.current },
    text: draft.text, cursor: draft.cursor,
    before: draft.text.slice(0, draft.cursor), after: draft.text.slice(draft.cursor),
    get value(): string { return current.current.text },
    get position(): number { return current.current.cursor },
    replace: (value: string, cursor?: number): void => {
      if (pending.current) return
      remember()
      commit(draftAt(composerText(value), cursor), undefined)
    },
    paste: (value: string): void => {
      if (pending.current) return
      remember()
      commit(insertText(current.current, value), undefined)
    },
    /**
     * Insert a terminal paste, collapsing long text into one placeholder.
     * @param value - the pasted text.
     */
    pasteBlock: (value: string): void => {
      if (pending.current) return
      remember()
      const text = composerText(value)
      if (!collapses(text)) { commit(insertText(current.current, text), undefined); return }
      const token = pastedTextToken(++pastes.current, text)
      atoms.current.set(token, { kind: 'text', text })
      commit(insertText(current.current, token), undefined)
    },
    /**
     * Insert the placeholder of a staged image at the cursor.
     * @param key - the attachment key erasing the placeholder reports.
     * @returns false when a submission holds the draft.
     */
    attach: (key: string): boolean => {
      if (pending.current) return false
      remember()
      const token = imageToken(++pastes.current)
      atoms.current.set(token, { kind: 'image', key })
      commit(insertText(current.current, token), undefined)
      return true
    },
    erase: (): void => { if (!pending.current) erase('backward') },
    /**
     * Apply an editing or caret key.
     *
     * By character: ←/→, Ctrl+B/F, Backspace, Delete, and Ctrl+D. By word:
     * Ctrl+←/→, Alt+←/→, and Alt+B/F move; Ctrl+W and Alt+Backspace kill back;
     * Alt+D, Alt+Delete, and Ctrl+Delete kill forward. Ctrl+U and Ctrl+K kill to
     * the logical line's start and end. Ctrl+Y yanks the last kill and Alt+Y
     * then cycles older ones. Ctrl+- (or Ctrl+_ or Ctrl+/) undoes. Home and End
     * reach the edge of the drawn row, then of the logical line; Ctrl+A and
     * Ctrl+E reach the logical line's.
     * @param text - the text Ink decoded from one read.
     * @param key - the key Ink decoded from it.
     * @param width - the columns the draft is wrapped at; absent, Home and End
     *   work on logical lines, as in a one-line field.
     * @returns whether the key was an editing key, applied or not.
     */
    editKey: (text: string, key: Key, width?: number): boolean => {
      if (pending.current) return true
      const { text: value, cursor } = current.current
      const word = (direction: 'left' | 'right'): void => { move(wordStop(value, cursor, direction, keys(), opaque)) }
      if (text === UNDO_KEY) { undoLast(); return true }
      if (key.meta) {
        if (key.leftArrow || text === 'b') word('left')
        else if (key.rightArrow || text === 'f') word('right')
        else if (key.backspace) killWord('left')
        else if (key.delete || text === 'd') killWord('right')
        else if (text === 'y') yankPop()
        else return false
        return true
      }
      if (key.ctrl && (key.leftArrow || key.rightArrow)) { word(key.leftArrow ? 'left' : 'right'); return true }
      if (key.ctrl && key.delete) { killWord('right'); return true }
      if (key.ctrl && !key.home && !key.end) {
        switch (text) {
          case 'w': killWord('left'); return true
          case 'u': killLine('left'); return true
          case 'k': killLine('right'); return true
          case 'y': yank(); return true
          case 'b': move(moveCursor(current.current, 'left', keys()).cursor); return true
          case 'f': move(moveCursor(current.current, 'right', keys()).cursor); return true
          case 'd': erase('forward'); return true
          case 'a': move(moveCursor(current.current, 'home').cursor); return true
          case 'e': move(moveCursor(current.current, 'end').cursor); return true
          default: break
        }
      }
      if (key.leftArrow || key.rightArrow) move(moveCursor(current.current, key.leftArrow ? 'left' : 'right', keys()).cursor)
      else if (key.home || key.end) {
        const edge = key.home ? 'start' : 'end'
        move(width === undefined || key.ctrl ? moveCursor(current.current, key.home ? 'home' : 'end').cursor
          : outsideAtoms(value, moveToRowEdge(current.current, width, edge).cursor, keys()))
      } else if (key.backspace || key.delete) erase(key.backspace ? 'backward' : 'forward')
      else return false
      return true
    },
    /**
     * Put the caret where a click landed.
     * @param offset - the draft offset drawn at the clicked cell; inside a
     *   placeholder, the caret goes before it.
     */
    place: (offset: number): void => {
      if (pending.current) return
      move(outsideAtoms(current.current.text, Math.min(offset, current.current.text.length), keys()))
    },
    /** Stop browsing history and restore the draft the browse started from. */
    leave: (): void => {
      if (visit.current === undefined) return
      last.current = undefined
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
      last.current = undefined
      show({ text: moved.draft.text, cursor: outsideAtoms(moved.draft.text, moved.draft.cursor, keys()) })
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
        // One undo step returns from a whole browse to the draft it started from.
        remember()
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
      last.current = undefined
      update(rows === undefined ? entry : draftAt(entry.text, recallCursor(entry, direction, rows(entry.text))))
      return true
    },
    type: (value: string): void => {
      if (pending.current) return
      const lines = composerText(value).split('\n')
      // Typing within a word is one undo step; whitespace starts the next.
      if (lines[0] !== '' && (last.current !== 'type' || /\s/u.test(lines[0]!))) remember()
      if (lines[0] !== '') last.current = 'type'
      let edited = insertText(current.current, lines[0]!)
      for (const line of lines.slice(1)) {
        if (edited.text.trim() !== '' || allowEmpty) {
          const accepted = accept(edited.text)
          if (accepted === false) { update(edited); return }
          if (accepted instanceof Promise) {
            pending.current = true
            setSubmitting(true)
            update(edited)
            const settle = (ok: boolean | void): void => {
              if (!mounted.current) return
              pending.current = false
              setSubmitting(false)
              if (ok !== false) { visit.current = undefined; forget(); update(draftAt('')) }
            }
            void accepted.then(settle, () => settle(false))
            return
          }
        }
        visit.current = undefined
        forget()
        edited = draftAt(line)
      }
      update(edited)
    },
  }
}
