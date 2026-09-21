/** Input integration against Ink's real key and paste channels. */
import React from 'react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { cleanup, render } from 'ink-testing-library'
import { App, type AppProps } from '../src/app.tsx'
import { dictionaries } from '../src/copy.ts'
import { questionAnswer } from '../src/interaction.tsx'

afterEach(cleanup)

function props(overrides: Partial<AppProps> = {}): AppProps {
  return {
    committed: [], live: [], pending: [], status: 'idle', stopping: false,
    command: undefined, notice: undefined, interaction: undefined,
    model: 'mock/model', cwd: '/workspace', sessionId: 'session-test', copy: dictionaries.en, context: undefined,
    onSubmit: vi.fn(), onCancel: vi.fn(), onInterrupt: vi.fn(), onAnswer: vi.fn(), ...overrides,
  }
}

describe('terminal composer', () => {
  it('submits typing and Enter delivered in one input read', async () => {
    const state = props()
    const ui = render(<App {...state} />)
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('Ready'))
    ui.stdin.write('/select  item \r')
    await vi.waitFor(() => expect(state.onSubmit).toHaveBeenCalledExactlyOnceWith('/select  item '))
  })

  it('keeps a fragmented multiline paste in the draft until Enter', async () => {
    const state = props()
    const ui = render(<App {...state} />)
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('Ready'))
    ui.stdin.write('\u001b[200~line one\n')
    ui.stdin.write('line two\u001b[201~')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('line two'))
    expect(state.onSubmit).not.toHaveBeenCalled()
    ui.stdin.write('\r')
    await vi.waitFor(() => expect(state.onSubmit).toHaveBeenCalledExactlyOnceWith('line one\nline two'))
  })

  it('does not approve a request from pasted Y', async () => {
    const state = props({ interaction: { kind: 'approval', id: 1, tool: 'bash', reason: 'Outside workspace' } })
    const ui = render(<App {...state} />)
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('Approval required'))
    ui.stdin.write('\u001b[200~Y\u001b[201~')
    ui.stdin.write('n\r')
    await vi.waitFor(() => expect(state.onAnswer).toHaveBeenCalledExactlyOnceWith(1, 'rejected'))
    expect(state.onSubmit).not.toHaveBeenCalled()
  })

  it('keeps login secrets out of rendered frames and the model composer', async () => {
    const state = props({ interaction: { kind: 'login', id: 2, message: 'API key', secret: true } })
    const ui = render(<App {...state} />)
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('API key'))
    ui.stdin.write('\u001b[200~private-test-value\u001b[201~')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('******************'))
    ui.stdin.write('\r')
    await vi.waitFor(() => expect(state.onAnswer).toHaveBeenCalledWith(2, 'private-test-value'))
    expect(ui.frames.join('')).not.toContain('private-test-value')
    expect(state.onSubmit).not.toHaveBeenCalled()
  })

  it.each(['en', 'zh'] as const)('shows pending input and interruption state in %s', async locale => {
    const copy = dictionaries[locale]
    const ui = render(<App {...props({ copy, status: 'running', stopping: true,
      pending: [{ id: 'input', target: 'next-step', text: 'Keep this instruction' }],
      live: [{ kind: 'assistant', text: 'Partial answer' }],
    })} />)
    await vi.waitFor(() => expect(ui.lastFrame()).toContain(copy.stopping))
    expect(ui.lastFrame()).toContain(copy.nextStep)
    expect(ui.lastFrame()).toContain('Keep this instruction')
    expect(ui.lastFrame()).toContain('Partial answer')
  })

  it('renders the complete plan and answers with the named option label', async () => {
    const state = props({ interaction: { id: 3, kind: 'questions', questions: [{
      id: 'plan', question: 'Review this plan', detail: '# Plan\nChange the adapter',
      options: [{ label: 'Revise' }, { label: 'Implement' }], intent: { kind: 'plan-review', approve: 'Implement' },
    }] } })
    const ui = render(<App {...state} />)
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('Change the adapter'))
    ui.stdin.write('2')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('> 2'))
    ui.stdin.write('\r')
    await vi.waitFor(() => expect(state.onAnswer).toHaveBeenCalledWith(3, { answers: [{ id: 'plan', selected: ['Implement'] }] }))
  })

  it('encodes multiple choices and free text without changing question ids', () => {
    const question = { id: 'choices', question: 'Choose', multiSelect: true, options: [{ label: 'A' }, { label: 'B' }] }
    expect(questionAnswer(question, '2,1')).toEqual({ id: 'choices', selected: ['B', 'A'] })
    expect(questionAnswer(question, 'Different approach')).toEqual({ id: 'choices', selected: [], custom: 'Different approach' })
  })
})
