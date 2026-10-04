/** Text transformations are independent of terminal and harness state. */
import { describe, expect, it } from 'bun:test'
import stringWidth from 'string-width'
import {
  atomRange, composerText, cursorWindow, draftRows, eraseLast, draftAt, insertText, moveCursor, moveToRowEdge, moveVertically, eraseAtCursor,
  offsetAt, outsideAtoms, TAB_COLUMNS, wordStop, wrapDraft,
} from '../src/editor.ts'

it('keeps pasted lines and tabs while removing terminal controls', () => {
  expect(composerText('a\r\nb\rc\t\u0003\u0000')).toBe('a\nb\nc\t')
})
it('keeps a narrow input window around the caret', () => {
  const window = cursorWindow('0123456789', 'abcdefghij', 10)
  expect(window.before).toContain('\u2026')
  expect(window.after).toContain('\u2026')
  expect(stringWidth(window.before) + 1 + stringWidth(window.after)).toBeLessThanOrEqual(10)
  expect(cursorWindow('', '0123456789', 5).before).toBe('')
  expect(cursorWindow('', '0123456789', 5).after).toContain('\u2026')
  // The caret covers the grapheme after it; at the end it covers nothing.
  expect(cursorWindow('ab', 'c👩🏽‍💻d', 20)).toEqual({ before: 'ab', under: 'c', after: '👩🏽‍💻d' })
  expect(cursorWindow('ab', '', 20)).toEqual({ before: 'ab', under: '', after: '' })
})

it('backspaces one visible grapheme', () => {
  expect(eraseLast('a👩🏽‍💻')).toBe('a')
  expect(eraseLast('ae\u0301')).toBe('a')
  expect(eraseLast('')).toBe('')
})

it('moves and deletes complete graphemes on either side of the cursor', () => {
  const text = 'a👩🏽‍💻e\u0301z'
  let draft = draftAt(text, 1)
  draft = moveCursor(draft, 'right')
  expect(draft.text.slice(0, draft.cursor)).toBe('a👩🏽‍💻')
  expect(eraseAtCursor(draft, 'forward').text).toBe('a👩🏽‍💻z')
  expect(eraseAtCursor(draft, 'backward')).toEqual({ text: 'ae\u0301z', cursor: 1 })
  expect(moveCursor(draft, 'left')).toEqual({ text, cursor: 1 })
  expect(moveCursor(draftAt(text, 0), 'left').cursor).toBe(0)
  expect(moveCursor(draftAt(text), 'right').cursor).toBe(text.length)
})

it('inserts literal multiline text in place and moves to logical line boundaries', () => {
  const draft = insertText(draftAt('first\nlast', 6), '中\r\nline\t')
  expect(draft).toEqual({ text: 'first\n中\nline\tlast', cursor: 13 })
  expect(moveCursor(draft, 'home').cursor).toBe(8)
  expect(moveCursor(draft, 'end').cursor).toBe(draft.text.length)
  expect(eraseAtCursor(draftAt('a\nb', 2), 'backward')).toEqual({ text: 'ab', cursor: 1 })
})

it('keeps the cursor outside graphemes formed by insertion', () => {
  expect(insertText(draftAt('👩💻', 2), '\u200d')).toEqual({ text: '👩‍💻', cursor: '👩‍💻'.length })
  expect(insertText(draftAt('ae\u0301b', 1), '中')).toEqual({ text: 'a中e\u0301b', cursor: 2 })
})

it.each(['น้ำ', 'กี้', 'ກຳ', 'ດີ', 'ភា', 'မြ', 'e\u0301'])('edits %s without separating its combining characters', cluster => {
  const text = `a${cluster}z`
  const end = 1 + cluster.length
  expect(draftAt(text, 2).cursor).toBe(end)
  expect(moveCursor(draftAt(text, 1), 'right').cursor).toBe(end)
  expect(moveCursor(draftAt(text, end), 'left').cursor).toBe(1)
  expect(eraseAtCursor(draftAt(text, end), 'backward')).toEqual({ text: 'az', cursor: 1 })
  expect(eraseAtCursor(draftAt(text, 1), 'forward')).toEqual({ text: 'az', cursor: 1 })
  let typed = draftAt('')
  for (const character of cluster) typed = insertText(typed, character)
  expect(typed).toEqual({ text: cluster, cursor: cluster.length })
})

describe('wrapDraft', () => {
  const wrap = (text: string, cursor: number, width: number) => wrapDraft(text, cursor, width, cell => `|${cell}`)

  it('breaks after whitespace and hangs it, so no wrapped row opens with a space', () => {
    // Laid out one column narrower than the row, which keeps the caret's column.
    expect(wrap('alpha beta gamma', 0, 11).rows).toEqual(['|alpha beta', 'gamma'])
    expect(wrap('alpha  beta   gamma', 0, 7).rows).toEqual(['|alpha ', 'beta  ', 'gamma'])
    for (let width = 4; width < 30; width++) {
      const { rows } = wrap(`${'word 你好 '.repeat(8)}end`, 0, width)
      for (const row of rows.slice(1)) expect(row.startsWith(' '), `${width}: ${JSON.stringify(rows)}`).toBe(false)
      for (const row of rows) expect(stringWidth(row)).toBeLessThanOrEqual(width)
    }
  })

  it('never reflows as the caret moves through the draft', () => {
    const text = 'alpha beta gamma delta epsilon 你好世界 zeta'
    const bare = (rows: readonly string[]) => rows.map(row => row.replace('|', ''))
    const expected = bare(wrap(text, 0, 12).rows)
    for (let cursor = 0; cursor <= text.length; cursor++) {
      const { rows, caret } = wrap(text, cursor, 12)
      expect(bare(rows)).toEqual(expected)
      expect(rows[caret]).toContain('|')
      expect(rows.join('').split('|')).toHaveLength(2)
    }
  })

  it('draws the caret where the next character goes, including after a full row', () => {
    expect(wrap('alpha beta', 10, 11)).toEqual({ rows: ['alpha beta|'], caret: 0 })
    expect(wrap('alpha beta gamma', 11, 11)).toEqual({ rows: ['alpha beta', '|gamma'], caret: 1 })
    // Inside the hanging space. The end of the row it hangs from.
    expect(wrap('alpha beta gamma', 10, 11)).toEqual({ rows: ['alpha beta|', 'gamma'], caret: 0 })
    expect(wrap('one\n\nthree', 4, 20)).toEqual({ rows: ['one', '|', 'three'], caret: 1 })
    expect(wrap('', 0, 20)).toEqual({ rows: ['|'], caret: 0 })
  })

  it('splits a word longer than the row where the row ends', () => {
    expect(wrap('ab abcdefghij', 13, 6).rows).toEqual(['ab ', 'abcde', 'fghij|'])
  })

  it('breaks between wide characters, which have no spaces to break at', () => {
    expect(wrap('你好世界你好', 0, 6).rows).toEqual(['|你好', '世界', '你好'])
    expect(wrap('ab你好', 0, 6).rows).toEqual(['|ab你', '好'])
  })

  it.each(['น้ำ', 'ກຳ'])('reserves both terminal cells of %s when wrapping', cluster => {
    const text = cluster.repeat(3)
    expect(stringWidth(cluster)).toBe(2)
    expect(wrap(text, text.length, 5).rows).toEqual([cluster.repeat(2), `${cluster}|`])
  })

  it.each([
    ['ภาษาไทยทดสอบ', 9, ['ภาษาไทย', 'ทดสอบ|']],
    ['ພາສາລາວທົດສອບ', 9, ['ພາສາລາວ', 'ທົດສອບ|']],
    ['ភាសាខ្មែរសាកល្បង', 11, ['ភាសាខ្មែរ', 'សាកល្បង|']],
    ['မြန်မာဘာသာစမ်းသပ်', 11, ['မြန်မာဘာသာ', 'စမ်းသပ်|']],
  ])('wraps %s at dictionary word boundaries without adding spaces', (text, width, rows) => {
    expect(wrap(text, text.length, width).rows).toEqual(rows)
  })

  it('preserves mixed scripts at every caret stop and when a word exceeds the row', () => {
    const text = 'ภาษาไทยน้ำກຳភាសាខ្មែរမြန်မာe\u0301👩🏽‍💻'
    const segments = new Intl.Segmenter('en', { granularity: 'grapheme' })
    const stops = [...segments.segment(text)].map(segment => segment.index).concat(text.length)
    for (const width of [5, 9, 20, 40]) {
      const expected = wrap(text, 0, width).rows.map(row => row.replace('|', ''))
      expect(expected.join('')).toBe(text)
      let offset = 0
      for (const row of expected) {
        expect(stops).toContain(offset)
        offset += row.length
      }
      for (const cursor of stops) {
        const { rows } = wrap(text, cursor, width)
        expect(rows.map(row => row.replace('|', ''))).toEqual(expected)
        expect(rows.join('').split('|')).toHaveLength(2)
        for (const row of rows) expect(stringWidth(row)).toBeLessThanOrEqual(width)
      }
    }
  })

  it('expands tabs to spaces, which is what the layout measured', () => {
    expect(wrap('\tx', 3, 20).rows).toEqual([`${' '.repeat(TAB_COLUMNS)}x|`])
    expect(wrap('ab\tx', 4, 20).rows).toEqual([`ab${' '.repeat(TAB_COLUMNS - 2)}x|`])
    expect(wrap('abc\tx', 0, 5).rows).toEqual(['|abc ', 'x'])
  })
})

describe('moveVertically', () => {
  // The caret's row and its offset within it, drawn as wrapDraft draws them.
  const shown = (text: string, cursor: number, width: number) => {
    const { rows, caret } = wrapDraft(text, cursor, width, cell => `|${cell}`)
    return `${caret}:${rows[caret]}`
  }
  const move = (text: string, cursor: number, width: number, direction: 'up' | 'down', goal?: number) =>
    moveVertically(draftAt(text, cursor), width, direction, goal)

  it('moves between logical lines and keeps the column', () => {
    const text = 'first line\nsecond line'
    const down = move(text, 3, 40, 'down')!
    expect(shown(text, down.draft.cursor, 40)).toBe('1:sec|ond line')
    expect(shown(text, move(text, down.draft.cursor, 40, 'up')!.draft.cursor, 40)).toBe('0:fir|st line')
  })

  it('moves between the rows of one wrapped line', () => {
    // 'alpha beta' then 'gamma', as wrapDraft draws it at 11 columns.
    const text = 'alpha beta gamma'
    expect(shown(text, move(text, 2, 11, 'down')!.draft.cursor, 11)).toBe('1:ga|mma')
    // Past the end of the last row, the caret goes after its text.
    expect(shown(text, move(text, 9, 11, 'down')!.draft.cursor, 11)).toBe('1:gamma|')
    expect(shown(text, move(text, text.length, 11, 'up')!.draft.cursor, 11)).toBe('0:alpha| beta')
    // A wrapped row's end is the next row's start, so its last place is before the hanging space.
    expect(shown(text, move(text, text.length, 11, 'up', 30)!.draft.cursor, 11)).toBe('0:alpha beta|')
  })

  it('reports the first and last rows, where history takes over', () => {
    expect(move('one\ntwo', 2, 40, 'up')).toBeUndefined()
    expect(move('one\ntwo', 6, 40, 'down')).toBeUndefined()
    expect(move('', 0, 40, 'up')).toBeUndefined()
    expect(move('one\ntwo', 6, 40, 'up')).toBeDefined()
  })

  it('never splits a wide character or a grapheme', () => {
    // Column 3 falls in the middle of 好, so the caret stops before it.
    const text = 'abcd\n你好'
    expect(shown(text, move(text, 3, 40, 'down')!.draft.cursor, 40)).toBe('1:你|好')
    const family = 'x👩🏽‍💻y\nabcdef'
    const up = move(family, family.length - 3, 40, 'up')!
    expect(up.draft.cursor % 1).toBe(0)
    expect(draftAt(family, up.draft.cursor)).toEqual(up.draft)
  })

  it.each(['น้ำ', 'ກຳ'])('counts both cells of %s during vertical movement', cluster => {
    const text = `abc\n${cluster}z`
    expect(shown(text, move(text, 1, 20, 'down')!.draft.cursor, 20)).toBe(`1:|${cluster}z`)
    expect(shown(text, move(text, 2, 20, 'down')!.draft.cursor, 20)).toBe(`1:${cluster}|z`)
  })

  it('keeps the goal column across a shorter row', () => {
    const text = 'long line here\nab\nanother long line'
    const middle = move(text, 10, 40, 'down')!
    expect(shown(text, middle.draft.cursor, 40)).toBe('1:ab|')
    const last = move(text, middle.draft.cursor, 40, 'down', middle.goal)!
    expect(shown(text, last.draft.cursor, 40)).toBe('2:another lo|ng line')
    expect(last.goal).toBe(10)
  })

  it('lands on empty lines and counts rows as they are drawn', () => {
    const text = 'one\n\nthree'
    expect(shown(text, move(text, 2, 40, 'down')!.draft.cursor, 40)).toBe('1:|')
    expect(draftRows(text, 40)).toBe(3)
    expect(draftRows('alpha beta gamma', 11)).toBe(2)
    expect(draftRows('', 11)).toBe(1)
  })
})

describe('wordStop', () => {
  /** Every stop a run of word steps makes, from one end of the text, drawn with `|`. */
  const walk = (text: string, direction: 'left' | 'right', atoms: readonly string[] = []) => {
    let cursor = direction === 'left' ? text.length : 0
    const stops: string[] = []
    for (;;) {
      const next = wordStop(text, cursor, direction, atoms)
      if (next === cursor) return stops
      cursor = next
      stops.push(`${text.slice(0, cursor)}|${text.slice(cursor)}`)
    }
  }

  it('skips whitespace, then one word', () => {
    expect(walk('hello  world', 'left')).toEqual(['hello  |world', '|hello  world'])
    expect(walk('hello  world', 'right')).toEqual(['hello|  world', 'hello  world|'])
  })

  it('stops at the punctuation inside a path, and steps over a run of it whole', () => {
    expect(walk('src/main.ts --fix', 'left')).toEqual([
      'src/main.ts --|fix', 'src/main.ts |--fix', 'src/main.|ts --fix', 'src/main|.ts --fix',
      'src/|main.ts --fix', 'src|/main.ts --fix', '|src/main.ts --fix',
    ])
    expect(walk('x  ...  y', 'right')).toEqual(['x|  ...  y', 'x  ...|  y', 'x  ...  y|'])
  })

  it('splits text without spaces at Unicode word boundaries', () => {
    expect(walk('你好世界 hi', 'left')).toEqual(['你好世界 |hi', '你好|世界 hi', '|你好世界 hi'])
  })

  it('crosses a line break alone, so a step never leaves its line otherwise', () => {
    expect(walk('one\ntwo', 'left')).toEqual(['one\n|two', 'one|\ntwo', '|one\ntwo'])
    expect(walk('one\ntwo', 'right')).toEqual(['one|\ntwo', 'one\n|two', 'one\ntwo|'])
  })

  it('crosses a placeholder whole', () => {
    const atoms = ['[Image #1]']
    expect(walk('a [Image #1] b', 'left', atoms)).toEqual(['a [Image #1] |b', 'a |[Image #1] b', '|a [Image #1] b'])
    expect(walk('a [Image #1] b', 'right', atoms)).toEqual(['a| [Image #1] b', 'a [Image #1]| b', 'a [Image #1] b|'])
  })

  it('treats a whole line as one word when opaque', () => {
    expect(wordStop('sk-live_abc def', 15, 'left', [], true)).toBe(0)
    expect(wordStop('sk-live_abc def', 0, 'right', [], true)).toBe(15)
  })
})

describe('placeholder ranges', () => {
  it('widens a range to every placeholder it touches, and moves a caret out of one', () => {
    const text = 'a [Image #1] b'
    expect(atomRange(text, 4, 13, ['[Image #1]'])).toEqual({ start: 2, end: 13 })
    expect(atomRange(text, 0, 5, ['[Image #1]'])).toEqual({ start: 0, end: 12 })
    expect(atomRange(text, 0, 1, ['[Image #1]'])).toEqual({ start: 0, end: 1 })
    expect(outsideAtoms(text, 5, ['[Image #1]'])).toBe(2)
    expect(outsideAtoms(text, 12, ['[Image #1]'])).toBe(12)
  })
})

describe('row edges and clicks', () => {
  // 'alpha beta' / 'gamma ' / 'delta', as wrapDraft draws it at 11 columns.
  const text = 'alpha beta gamma delta'
  const shown = (cursor: number) => wrapDraft(text, cursor, 11, cell => `|${cell}`).rows.join(' / ')

  it('reaches the edge of the drawn row, then of the logical line', () => {
    expect(shown(moveToRowEdge(draftAt(text, 13), 11, 'start').cursor)).toBe('alpha beta / |gamma  / delta')
    // A wrapped row's last place is the whitespace hanging past it.
    expect(shown(moveToRowEdge(draftAt(text, 13), 11, 'end').cursor)).toBe('alpha beta / gamma|  / delta')
    expect(moveToRowEdge(draftAt(text, 11), 11, 'start').cursor).toBe(0)
    expect(moveToRowEdge(draftAt(text, 16), 11, 'end').cursor).toBe(text.length)
    expect(moveToRowEdge(draftAt('one\ntwo', 5), 40, 'start').cursor).toBe(4)
    expect(moveToRowEdge(draftAt('one\ntwo', 4), 40, 'start').cursor).toBe(4)
  })

  it('maps a cell of a drawn row to the offset before the grapheme there', () => {
    expect(offsetAt(text, 11, 1, 2)).toBe(13)
    expect(offsetAt(text, 11, 0, 50)).toBe(10)
    expect(offsetAt(text, 11, 2, 50)).toBe(text.length)
    expect(offsetAt(text, 11, 3, 0)).toBeUndefined()
    // Column 1 is the second cell of 你, which a click places the caret before.
    expect(offsetAt('你好', 40, 0, 1)).toBe(0)
    expect(offsetAt('你好', 40, 0, 2)).toBe(1)
  })
})
