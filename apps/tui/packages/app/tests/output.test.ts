/** Rewriting a render's writes so the terminal never shows the controls erased, and draws them on the bottom row. */
import { describe, expect, test } from 'bun:test'
import { anchor, overwrite, scrolling } from '../src/output.ts'

const ESC = '\u001B['
/** `ansi-escapes` `eraseLines(rows)`, byte for byte. */
const eraseLines = (rows: number) => `${`${ESC}2K${ESC}1A`.repeat(rows - 1)}${ESC}2K${ESC}G`

describe('overwrite', () => {
  test('draws a printed row and the frame over the old frame instead of erasing it', () => {
    expect(overwrite([eraseLines(4), 'printed\n', 'frame one\nframe two\n'])).toBe(
      `${ESC}3A${ESC}G${ESC}K`
      + `printed\n${ESC}K`
      + `frame one\n${ESC}K`
      + `frame two\n${ESC}K`
      + `${ESC}J`,
    )
  })

  test('moves nowhere to overwrite a one-row region', () => {
    expect(overwrite([eraseLines(1), 'row\n'])).toBe(`${ESC}G${ESC}Krow\n${ESC}K${ESC}J`)
  })

  test('completes the frame inside a synchronized update', () => {
    const output = overwrite([`${ESC}?2026h`, eraseLines(2), 'row\n', `${ESC}?2026l`])
    expect(output).toBe(`${ESC}?2026h${ESC}1A${ESC}G${ESC}Krow\n${ESC}K${ESC}J${ESC}?2026l`)
  })

  test('closes an overwrite before anything that moves the cursor between rows', () => {
    const next = `${ESC}2Ashifted\n`
    expect(overwrite([eraseLines(2), 'row\n', next])).toBe(`${ESC}1A${ESC}G${ESC}Krow\n${ESC}K${ESC}J${next}`)
  })

  test('clears the region when nothing is drawn after the erase', () => {
    expect(overwrite([eraseLines(3)])).toBe(`${ESC}2A${ESC}G${ESC}K${ESC}J`)
  })

  test('passes through writes it does not recognize', () => {
    const incremental = `${ESC}2A${ESC}E${ESC}1Gchanged${ESC}K\n`
    const clear = `${ESC}2J${ESC}3J${ESC}Hhistory\nframe\n`
    expect(overwrite([incremental])).toBe(incremental)
    expect(overwrite([clear])).toBe(clear)
    expect(overwrite(['\u001B[?25l', 'plain\n'])).toBe('\u001B[?25lplain\n')
    // An erase with anything else in the same write is not the region clear.
    expect(overwrite([`${eraseLines(2)}text`])).toBe(`${eraseLines(2)}text`)
  })
})

describe('scrolling', () => {
  test('steps over an unchanged row with a newline, which a terminal scrolls at its bottom row', () => {
    const incremental = `${ESC}3A${ESC}E${ESC}1Gchanged${ESC}K\n${ESC}E${ESC}E`
    expect(scrolling(incremental)).toBe(`${ESC}3A\r\n${ESC}1Gchanged${ESC}K\n\r\n\r\n`)
  })

  test('leaves every other move as it is', () => {
    const moves = `${ESC}2A${ESC}G${ESC}K${ESC}J${ESC}24B${ESC}2J${ESC}3J${ESC}H`
    expect(scrolling(moves)).toBe(moves)
  })
})

describe('anchor', () => {
  // `ansi-escapes` `clearTerminal`, which starts Ink's replay of history.
  const clear = `${ESC}2J${ESC}3J${ESC}H`

  test('starts each replay of history on the bottom row', () => {
    expect(anchor(`${clear}history\nframe\n`, 24)).toBe(`${clear}${ESC}24Bhistory\nframe\n`)
    expect(anchor(`${clear}a\n${clear}b\n`, 10)).toBe(`${clear}${ESC}10Ba\n${clear}${ESC}10Bb\n`)
  })

  test('leaves a render without a clear, or a stream without a height, as it is', () => {
    expect(anchor(`${eraseLines(2)}frame\n`, 24)).toBe(`${eraseLines(2)}frame\n`)
    expect(anchor(`${clear}history\n`, undefined)).toBe(`${clear}history\n`)
  })
})
