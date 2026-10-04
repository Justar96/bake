/** Prompt navigation finds turn boundaries from row kinds alone. */
import { expect, test } from 'bun:test'
import type { Row } from '../src/rows.ts'
import { appendTranscript, emptyTranscript } from '../src/transcript.ts'
import { Viewport } from '../src/viewport.ts'

const user = (text: string): Row => ({ kind: 'user', text })
const answer = (text: string): Row => ({ kind: 'assistant', text })

/** Row 0 is the heading; prompts sit at rows 1, 3, and the live row 5. */
function viewport(): Viewport {
  const view = new Viewport('session')
  view.update(appendTranscript(emptyTranscript, [user('first'), answer('one'), user('second'), answer('two')]), [user('live'), answer('streaming')])
  return view
}

test('steps to the previous prompt, reaching the start of the prompt the view is inside', () => {
  const view = viewport()
  expect(view.prompt(view.end, -1)).toEqual({ row: 5, offset: 0 })
  expect(view.prompt({ row: 5, offset: 0 }, -1)).toEqual({ row: 3, offset: 0 })
  expect(view.prompt({ row: 4, offset: 2 }, -1)).toEqual({ row: 3, offset: 0 })
  expect(view.prompt({ row: 3, offset: 1 }, -1)).toEqual({ row: 3, offset: 0 })
  expect(view.prompt({ row: 1, offset: 0 }, -1)).toBeUndefined()
})

test('steps to the next prompt, through live rows, and finds none past the last', () => {
  const view = viewport()
  expect(view.prompt({ row: 0, offset: 0 }, 1)).toEqual({ row: 1, offset: 0 })
  expect(view.prompt({ row: 1, offset: 2 }, 1)).toEqual({ row: 3, offset: 0 })
  expect(view.prompt({ row: 3, offset: 0 }, 1)).toEqual({ row: 5, offset: 0 })
  expect(view.prompt({ row: 5, offset: 0 }, 1)).toBeUndefined()
  expect(view.prompt(view.end, 1)).toBeUndefined()
})

test('reads no row text', () => {
  const rows = Array.from({ length: 1000 }, (_, index): Row => index % 100 === 0
    ? { kind: 'user', get text(): string { throw new Error(`read prompt ${index}`) } }
    : { kind: 'assistant', get text(): string { throw new Error(`read answer ${index}`) } })
  const view = new Viewport('session')
  view.update(appendTranscript(emptyTranscript, rows), [])
  expect(view.prompt(view.end, -1)).toEqual({ row: 901, offset: 0 })
  expect(view.prompt({ row: 901, offset: 0 }, -1)).toEqual({ row: 801, offset: 0 })
})
