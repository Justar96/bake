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
  test('gives skills, subagents, messages to them, and the task list their own shape', () => {
    expect(iconFor('skill')).toBe(ICON.skill)
    expect(iconFor('subagent')).toBe(ICON.spawn)
    expect(iconFor('subagent_fork')).toBe(ICON.spawn)
    expect(iconFor('send_message')).toBe(ICON.send)
    expect(iconFor('todo_write')).toBe(ICON.todo)
  })

  test('keeps the plain dot for every other tool Bake registers', () => {
    for (const tool of ['bash', 'pwsh', 'read', 'edit', 'write', 'grep', 'glob', 'web_fetch', 'web_search', 'job_output',
      'job_kill', 'interrupt_agent', 'list_agents', 'workflow', 'exit_plan_mode', 'ask_user_question', 'get_goal', 'present']) {
      expect(iconFor(tool)).toBe(MARKER.action)
    }
  })

  test('guesses a plugin tool from whole words of its name, and falls back to the action marker', () => {
    expect(iconFor('SpawnWorker')).toBe(ICON.spawn)
    expect(iconFor('message_agent')).toBe(ICON.send)
    expect(iconFor('load_skill')).toBe(ICON.skill)
    expect(iconFor('update_todos')).toBe(ICON.todo)
    // One word of a family is not enough, and neither is part of a word.
    expect(iconFor('send_email')).toBe(MARKER.action)
    expect(iconFor('skillet')).toBe(MARKER.action)
    expect(iconFor('mcp__github__search_issues')).toBe(MARKER.action)
  })
})
