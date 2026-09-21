/** Draft editing for Ink's separate typed-input and paste channels. */
import { useRef, useState } from 'react'
import { composerText, eraseLast } from './editor.ts'

/**
 * Keep same-read edits available before React paints the next frame.
 * @param submit - explicit Enter action; receives one non-empty draft.
 * @returns the draft plus paste, typing, and deletion actions.
 */
export function useComposer(submit: (text: string) => void) {
  const [text, setText] = useState('')
  const current = useRef('')
  const update = (value: string): void => { current.current = value; setText(value) }
  return {
    text,
    paste: (value: string): void => update(current.current + composerText(value)),
    erase: (): void => update(eraseLast(current.current)),
    type: (value: string): void => {
      const lines = composerText(value).split('\n')
      let draft = current.current + lines[0]!
      for (const line of lines.slice(1)) {
        if (draft.trim() !== '') submit(draft)
        draft = line
      }
      update(draft)
    },
  }
}
