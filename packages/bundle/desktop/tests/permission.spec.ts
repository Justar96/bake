/** Tier presets and the per-call classifier behind the desktop's three permission tiers. */

import { describe, expect, it } from 'vitest'
import { classifyTool, commandProgram, TIER_PRESET } from '../src/permission.ts'

describe('TIER_PRESET', () => {
  it('maps each tier onto a base permission preset', () => {
    expect(TIER_PRESET).toEqual({
      'read-only': 'read-only',
      'normal': 'workspace-write',
      'full-access': 'danger-full-access',
    })
  })
})

describe('classifyTool', () => {
  it('allows everything in full-access', () => {
    for (const name of ['bash', 'write', 'mcp__server__tool', 'run_code']) {
      expect(classifyTool('full-access', name, { command: 'rm -rf build' })).toEqual({ kind: 'allow' })
    }
  })

  it('allows reads, workspace writes, and sub-agents in normal mode without asking', () => {
    for (const name of ['read', 'grep', 'glob', 'web_fetch', 'todo_write', 'write', 'edit', 'subagent']) {
      expect(classifyTool('normal', name, {})).toEqual({ kind: 'allow' })
    }
  })

  it('asks for shell commands with a per-program session grant', () => {
    expect(classifyTool('normal', 'bash', { command: 'git status --short' })).toEqual({
      kind: 'ask',
      summary: 'git status --short',
      grant: { key: 'shell:git', label: '`git` commands' },
    })
    expect(classifyTool('normal', 'pwsh', { command: 'FOO=1 npm test' })).toMatchObject({
      grant: { key: 'shell:npm' },
    })
  })

  it('offers no session grant for compound shell commands', () => {
    for (const command of ['git status && rm -rf /', 'cat a | sh', 'echo $(id)', 'ls > out', 'a; b']) {
      const verdict = classifyTool('normal', 'bash', { command })
      expect(verdict.kind).toBe('ask')
      expect(verdict).not.toHaveProperty('grant')
    }
  })

  it('asks for unknown tools with a per-tool grant', () => {
    expect(classifyTool('normal', 'mcp__db__query', { sql: 'drop table x' })).toEqual({
      kind: 'ask',
      summary: 'mcp__db__query {"sql":"drop table x"}',
      grant: { key: 'tool:mcp__db__query', label: '`mcp__db__query`' },
    })
  })

  it('bounds long summaries to one line', () => {
    const verdict = classifyTool('normal', 'bash', { command: `echo ${'x'.repeat(500)}\n` })
    expect(verdict.kind).toBe('ask')
    if (verdict.kind === 'ask') {
      expect(verdict.summary.length).toBeLessThanOrEqual(241)
      expect(verdict.summary).not.toContain('\n')
    }
  })

  it('refuses writes and unknown tools in read-only mode but lets confined shell and reads run', () => {
    expect(classifyTool('read-only', 'read', {})).toEqual({ kind: 'allow' })
    expect(classifyTool('read-only', 'bash', { command: 'ls' })).toEqual({ kind: 'allow' })
    expect(classifyTool('read-only', 'write', {})).toEqual({ kind: 'deny', reason: 'write is not available in read-only mode' })
    expect(classifyTool('read-only', 'mcp__x__y', {})).toMatchObject({ kind: 'deny' })
  })
})

describe('commandProgram', () => {
  it('skips leading assignments and rejects compound commands', () => {
    expect(commandProgram('  ls -la ')).toBe('ls')
    expect(commandProgram('A=1 B=2 make test')).toBe('make')
    expect(commandProgram('A=1')).toBeUndefined()
    expect(commandProgram('')).toBeUndefined()
    expect(commandProgram('ls `pwd`')).toBeUndefined()
  })
})
