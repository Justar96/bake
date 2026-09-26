/** Rail icons for each kind of action. */
import { describe, expect, test } from 'bun:test'
import stringWidth from 'string-width'
import { ICON, iconFor } from '../src/icons.ts'
import { MARKER, TREE } from '../src/layout.ts'

describe('ICON', () => {
  test('every icon is one cell with no emoji presentation', () => {
    // The rail is one glyph and a space. A wide icon shifts its row's text.
    for (const icon of Object.values(ICON)) {
      expect(stringWidth(icon)).toBe(1)
      expect(/\p{Emoji_Presentation}/u.test(icon)).toBe(false)
    }
  })

  test('no icon reads as a prompt, a selection, or the tree', () => {
    const reserved: readonly string[] = [MARKER.prompt, MARKER.selected, MARKER.current, MARKER.waiting, ...Object.values(TREE)]
    for (const icon of Object.values(ICON)) expect(reserved).not.toContain(icon)
  })
})

describe('iconFor', () => {
  test('names the tools Bake registers', () => {
    expect(iconFor('bash')).toBe(ICON.run)
    expect(iconFor('read')).toBe(ICON.read)
    expect(iconFor('edit')).toBe(ICON.edit)
    expect(iconFor('grep')).toBe(ICON.find)
    expect(iconFor('web_fetch')).toBe(ICON.fetch)
    expect(iconFor('subagent')).toBe(ICON.spawn)
    expect(iconFor('send_message')).toBe(ICON.send)
    expect(iconFor('job_output')).toBe(ICON.receive)
    expect(iconFor('interrupt_agent')).toBe(ICON.stop)
  })

  test('guesses a plugin tool from its name, and falls back to the action marker', () => {
    expect(iconFor('mcp__github__search_issues')).toBe(ICON.mcp)
    expect(iconFor('ReadFile')).toBe(ICON.read)
    expect(iconFor('frobnicate')).toBe(MARKER.action)
  })
})
