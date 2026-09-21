/** Input integration against Ink's real key and paste channels. */
import React from 'react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { cleanup, render } from 'ink-testing-library'
import { App, type AppProps } from '../src/app.tsx'
import { appendTranscript, emptyTranscript } from '../src/transcript.ts'
import { dictionaries } from '../src/copy.ts'
import { questionAnswer } from '../src/interaction.tsx'

afterEach(cleanup)

function props(overrides: Partial<AppProps> = {}): AppProps {
  return {
    files: { query: undefined, entries: [], loading: false, error: undefined }, onReferenceQuery: () => {},
    completion: { entries: [], loading: false, error: undefined }, completionLimit: 8,
    committed: emptyTranscript, live: [], pending: [], status: 'idle', stopping: false,
    command: undefined, notice: undefined, interaction: undefined,
    model: 'mock/model', cwd: '/workspace', sessionId: 'session-test', copy: dictionaries.en, context: undefined,
    onSubmit: vi.fn(), onCancel: vi.fn(), onInterrupt: vi.fn(), onAnswer: vi.fn(), ...overrides,
  }
}

describe('terminal composer', () => {
  it.each(['en', 'zh'] as const)('completes a selected skill without submitting in %s', async locale => {
    const state = props({ copy: dictionaries[locale], completionLimit: 2, completion: { loading: false, error: undefined, entries: [
      { name: 'reset', description: 'Reset the view', kind: 'command' },
      { name: 'review', description: 'Review the current patch', kind: 'skill' },
      { name: 'rewrite', description: 'Rewrite prose', kind: 'skill' },
    ] } })
    const ui = render(<App {...state} />)
    ui.stdin.write('/r')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('/review'))
    expect(ui.lastFrame()).not.toContain('/rewrite')
    ui.stdin.write('\u001b[B')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('› /review'))
    await expect(ui.lastFrame() + '\n').toMatchFileSnapshot(`./expected/slash-menu.${locale}.txt`)
    ui.stdin.write('\t')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('> /review ▌'))
    expect(ui.lastFrame()).not.toContain(state.copy.completionTitle)
    expect(state.onSubmit).not.toHaveBeenCalled()
    ui.stdin.write('this patch\r')
    await vi.waitFor(() => expect(state.onSubmit).toHaveBeenCalledExactlyOnceWith('/review this patch'))
  })

  it('closes the slash menu before interrupting the agent and reopens it after editing', async () => {
    const state = props({ completion: { entries: [{ name: 'help', description: 'List commands', kind: 'command' }], loading: false, error: undefined } })
    const ui = render(<App {...state} />)
    ui.stdin.write('/')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('› /help'))
    ui.stdin.write('\u001b')
    await vi.waitFor(() => expect(ui.lastFrame()).not.toContain(state.copy.completionTitle))
    expect(state.onCancel).not.toHaveBeenCalled()
    ui.stdin.write('\u001b')
    await vi.waitFor(() => expect(state.onCancel).toHaveBeenCalledTimes(1))
    ui.stdin.write('he')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('› /help'))
    ui.stdin.write('\t\r')
    await vi.waitFor(() => expect(state.onSubmit).toHaveBeenCalledExactlyOnceWith('/help '))
  })

  it('keeps pasted slash input in the draft and shows discovery failures without blocking typing', async () => {
    const state = props({ completion: { entries: [], loading: true, error: undefined } })
    const ui = render(<App {...state} />)
    ui.stdin.write('\u001b[200~/review\u001b[201~')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain(state.copy.catalogLoading))
    expect(state.onSubmit).not.toHaveBeenCalled()
    ui.rerender(<App {...state} completion={{ entries: [], loading: false, error: 'Provider unavailable' }} />)
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('Provider unavailable'))
    ui.stdin.write(' this change\r')
    await vi.waitFor(() => expect(state.onSubmit).toHaveBeenCalledExactlyOnceWith('/review this change'))
  })

  it.each(['en', 'zh'] as const)('browses directories and completes a quoted file in %s', async locale => {
    const query = vi.fn()
    const state = props({ copy: dictionaries[locale], onReferenceQuery: query,
      files: { query: 'notes', loading: false, error: undefined, entries: [{ path: 'notes folder', kind: 'directory' }] },
    })
    const ui = render(<App {...state} />)
    ui.stdin.write('Review @notes')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('› @"notes folder/'))
    expect(query).toHaveBeenLastCalledWith('notes')
    ui.stdin.write('\t')
    await vi.waitFor(() => expect(query).toHaveBeenLastCalledWith('notes folder/'))
    expect(ui.lastFrame()).toContain('> Review @"notes folder/▌')
    expect(state.onSubmit).not.toHaveBeenCalled()
    ui.rerender(<App {...state} files={{ query: 'notes folder/', loading: false, error: undefined, entries: [
      { path: 'notes folder/read me.txt', kind: 'file' }, { path: 'notes folder/todo.txt', kind: 'file' },
    ] }} />)
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('› @"notes folder/read me.txt"'))
    await expect(ui.lastFrame() + '\n').toMatchFileSnapshot(`./expected/file-menu.${locale}.txt`)
    ui.stdin.write('\t\r')
    await vi.waitFor(() => expect(state.onSubmit).toHaveBeenCalledExactlyOnceWith('Review @"notes folder/read me.txt" '))
    await vi.waitFor(() => expect(query).toHaveBeenLastCalledWith(undefined))
  })

  it('never inserts paths from an earlier query and leaves pasted tabs and newlines literal', async () => {
    const query = vi.fn()
    const state = props({ onReferenceQuery: query,
      files: { query: 'old', loading: false, error: undefined, entries: [{ path: 'old.txt', kind: 'file' }] },
    })
    const ui = render(<App {...state} />)
    ui.stdin.write('Review @new\t')
    await vi.waitFor(() => expect(query).toHaveBeenLastCalledWith('new'))
    expect(ui.lastFrame()).toContain(state.copy.filesLoading)
    expect(ui.lastFrame()).not.toContain('old.txt')
    ui.stdin.write('\u001b')
    await vi.waitFor(() => expect(query).toHaveBeenLastCalledWith(undefined))
    expect(state.onCancel).not.toHaveBeenCalled()
    ui.stdin.write('\u001b')
    await vi.waitFor(() => expect(state.onCancel).toHaveBeenCalledTimes(1))
    ui.stdin.write('er')
    await vi.waitFor(() => expect(query).toHaveBeenLastCalledWith('newer'))
    ui.rerender(<App {...state} files={{ query: 'newer', loading: false, error: 'Search unavailable', entries: [] }} />)
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('Search unavailable'))
    ui.stdin.write('\u001b[200~\t\nuser@example.com\u001b[201~')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('user@example.com'))
    expect(ui.lastFrame()).not.toContain(state.copy.filesTitle)
    expect(state.onSubmit).not.toHaveBeenCalled()
    ui.stdin.write('\r')
    await vi.waitFor(() => expect(state.onSubmit).toHaveBeenCalledExactlyOnceWith('Review @newer\t\nuser@example.com'))
  })

  it.each(['en', 'zh'] as const)('filters model choices and accepts only explicit Enter in %s', async locale => {
    const state = props({ copy: dictionaries[locale], completionLimit: 2, interaction: {
      id: 10, kind: 'select', title: dictionaries[locale].chooseModel, initial: 'mock/current', choices: [
        { value: 'mock/current', label: 'mock/current', current: true },
        { value: 'mock/other', label: 'mock/other' },
        { value: 'provider/model', label: 'provider/model' },
      ],
    } })
    const ui = render(<App {...state} />)
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('› mock/current'))
    expect(ui.lastFrame()).not.toContain(state.copy.help)
    expect(ui.lastFrame()).not.toContain('provider/model')
    ui.stdin.write('\u001b[B')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('› mock/other'))
    await expect(ui.lastFrame() + '\n').toMatchFileSnapshot(`./expected/model-picker.${locale}.txt`)
    ui.stdin.write('\u001b[200~provider\u001b[201~')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('› provider/model'))
    expect(state.onAnswer).not.toHaveBeenCalled()
    ui.stdin.write('\r')
    await vi.waitFor(() => expect(state.onAnswer).toHaveBeenCalledExactlyOnceWith(10, 'provider/model'))
    ui.stdin.write('\r')
    expect(state.onAnswer).toHaveBeenCalledTimes(1)
    expect(state.onSubmit).not.toHaveBeenCalled()
  })

  it.each(['en', 'zh'] as const)('shows current effort and the provider default in %s', async locale => {
    const copy = dictionaries[locale]
    const state = props({ copy, interaction: { id: 11, kind: 'select', title: copy.chooseEffort, initial: 'high', choices: [
      { value: '', label: copy.providerDefault, description: 'low' },
      { value: 'low', label: 'Low' }, { value: 'high', label: 'High', current: true },
    ] } })
    const ui = render(<App {...state} />)
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('› High'))
    await expect(ui.lastFrame() + '\n').toMatchFileSnapshot(`./expected/effort-picker.${locale}.txt`)
    ui.stdin.write('\u001b[B')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain(`› ${copy.providerDefault}`))
    ui.stdin.write('\r')
    await vi.waitFor(() => expect(state.onAnswer).toHaveBeenCalledExactlyOnceWith(11, ''))
  })

  it('keeps an empty filtered picker open and routes Escape to cancellation', async () => {
    const state = props({ interaction: { id: 12, kind: 'select', title: 'Choose model', initial: 'mock/model',
      warning: 'Unavailable model catalogs: offline', choices: [{ value: 'mock/model', label: 'mock/model' }],
    } })
    const ui = render(<App {...state} />)
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('Unavailable model catalogs: offline'))
    ui.stdin.write('missing\r')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain(state.copy.noChoices))
    expect(state.onAnswer).not.toHaveBeenCalled()
    expect(state.onSubmit).not.toHaveBeenCalled()
    ui.stdin.write('\u001b')
    await vi.waitFor(() => expect(state.onCancel).toHaveBeenCalledTimes(1))
  })

  it('edits at grapheme boundaries and distinguishes Backspace from forward Delete', async () => {
    const state = props()
    const ui = render(<App {...state} />)
    ui.stdin.write('a👩🏽‍💻b')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('> a👩🏽‍💻b▌'))
    ui.stdin.write('\u001b[D')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('> a👩🏽‍💻▌b'))
    ui.stdin.write('\u007f')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('> a▌b'))
    ui.stdin.write('\u001b[200~中\u001b[201~')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('> a中▌b'))
    ui.stdin.write('\u001b[H')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('> ▌a中b'))
    ui.stdin.write('\u001b[3~')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('> ▌中b'))
    ui.stdin.write('\u001b[C')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('> 中▌b'))
    ui.stdin.write('z\r')
    await vi.waitFor(() => expect(state.onSubmit).toHaveBeenCalledExactlyOnceWith('中zb'))
  })

  it('inserts Shift-Enter and multiline paste at the cursor without submitting', async () => {
    const state = props()
    const ui = render(<App {...state} />)
    ui.stdin.write('first')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('> first▌'))
    ui.stdin.write('\u001b[13;2u')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('first\n▌'))
    ui.stdin.write('last')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('last▌'))
    ui.stdin.write('\u0001')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('▌last'))
    ui.stdin.write('\u001b[200~X\nY\u001b[201~')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('Y▌last'))
    expect(state.onSubmit).not.toHaveBeenCalled()
    ui.stdin.write('\u0005')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('Ylast▌'))
    ui.stdin.write('\u001b[D')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('Ylas▌t'))
    ui.stdin.write('\r')
    await vi.waitFor(() => expect(state.onSubmit).toHaveBeenCalledExactlyOnceWith('first\nX\nYlast'))
  })

  it.each(['en', 'zh'] as const)('preserves recalled edits and restores the unsent draft and cursor in %s', async locale => {
    const committed = appendTranscript(emptyTranscript, [
      { kind: 'user', text: 'First prompt' }, { kind: 'assistant', text: 'Do not recall this answer' },
      { kind: 'user', text: 'Second prompt' },
    ])
    const state = props({ committed, copy: dictionaries[locale], pending: [{ id: 'queued', target: 'next-step', text: 'Queued prompt' }] })
    const ui = render(<App {...state} />)
    ui.stdin.write('Unsent draft')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('> Unsent draft▌'))
    ui.stdin.write('\u001b[D')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('> Unsent draf▌t'))
    ui.stdin.write('\u001b[A')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('> Queued prompt▌'))
    ui.stdin.write('\u001b[A')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('> Second prompt▌'))
    ui.stdin.write(' edited')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('> Second prompt edited▌'))
    ui.stdin.write('\u001b[A')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('> First prompt▌'))
    ui.stdin.write('\u001b[B')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('> Second prompt edited▌'))
    ui.stdin.write('\u001b[B')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('> Queued prompt▌'))
    ui.stdin.write('\u001b[B')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('> Unsent draf▌t'))
    await expect(ui.lastFrame() + '\n').toMatchFileSnapshot(`./expected/composer-history.${locale}.txt`)
    expect(state.onSubmit).not.toHaveBeenCalled()
    expect(committed.rows.at(-1)).toEqual({ kind: 'user', text: 'Second prompt' })
    ui.stdin.write('!\r')
    await vi.waitFor(() => expect(state.onSubmit).toHaveBeenCalledExactlyOnceWith('Unsent draf!t'))
  })

  it('browses recalled slash input without opening completion and picks up new history after restoring the draft', async () => {
    const state = props({ completion: { loading: false, error: undefined, entries: [{ name: 'help', description: 'Help', kind: 'command' }] } })
    const ui = render(<App {...state} />)
    ui.stdin.write('\u001b[A')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('> ▌'))
    const committed = appendTranscript(emptyTranscript, [{ kind: 'user', text: 'First' }, { kind: 'user', text: '/he' }])
    ui.rerender(<App {...state} committed={committed} />)
    ui.stdin.write('\u001b[A')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('> /he▌'))
    expect(ui.lastFrame()).not.toContain(state.copy.completionTitle)
    ui.stdin.write('\u0010')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('> First▌'))
    ui.stdin.write('\u000e')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('> /he▌'))
    ui.stdin.write('\u001b[D')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('› /help'))
    ui.stdin.write('\t\r')
    await vi.waitFor(() => expect(state.onSubmit).toHaveBeenCalledExactlyOnceWith('/help '))
  })

  it('completes a file at the cursor and submits the untouched suffix', async () => {
    const query = vi.fn()
    const state = props({ onReferenceQuery: query,
      files: { query: 'ol', loading: false, error: undefined, entries: [{ path: 'new file.txt', kind: 'file' }] },
    })
    const ui = render(<App {...state} />)
    ui.stdin.write('Read @old.txt later')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('> Read @old.txt later▌'))
    ui.stdin.write('\u001b[H')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('> ▌Read @old.txt later'))
    for (let index = 0; index < 'Read @ol'.length; index++) ui.stdin.write('\u001b[C')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('> Read @ol▌d.txt later'))
    expect(query).toHaveBeenLastCalledWith('ol')
    ui.stdin.write('\t')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('> Read @"new file.txt" ▌later'))
    ui.stdin.write('\r')
    await vi.waitFor(() => expect(state.onSubmit).toHaveBeenCalledExactlyOnceWith('Read @"new file.txt" later'))
  })

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
    ui.rerender(<App {...state} interaction={undefined} />)
    ui.stdin.write('\u001b[A')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('> ▌'))
    expect(ui.lastFrame()).not.toContain('private-test-value')
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

it.each(['en', 'zh'] as const)('renders a bounded session picker and returns the selected id in %s', async locale => {
  const copy = dictionaries[locale]
  const state = props({ copy, inputBlocked: true, completionLimit: 2,
    interaction: { kind: 'select', id: 10, title: copy.chooseSession, initial: 'session-first', choices: [
      { value: '', label: copy.newSession },
      { value: 'session-first', label: 'First conversation', current: true },
      { value: 'session-second', label: 'Second conversation' },
    ] },
  })
  const ui = render(<App {...state} />)
  await vi.waitFor(() => expect(ui.lastFrame()).toContain(copy.chooseSession))
  await expect(ui.lastFrame() + '\n').toMatchFileSnapshot(`./expected/session-picker.${locale}.txt`)
  ui.stdin.write('\u001b[200~second\u001b[201~')
  await vi.waitFor(() => expect(ui.lastFrame()).toContain('› Second conversation'))
  expect(state.onAnswer).not.toHaveBeenCalled()
  ui.stdin.write('\r')
  await vi.waitFor(() => expect(state.onAnswer).toHaveBeenCalledExactlyOnceWith(10, 'session-second'))
  expect(state.onSubmit).not.toHaveBeenCalled()
})

it('resets draft, cursor, recall, and printed history when the displayed session changes', async () => {
  const state = props({ committed: appendTranscript(emptyTranscript, [{ kind: 'user', text: 'First saved input' }]) })
  const ui = render(<App {...state} />)
  ui.stdin.write('Unsent draft')
  ui.stdin.write('\u001b[D')
  ui.stdin.write('\u001b[A')
  await vi.waitFor(() => expect(ui.lastFrame()).toContain('> First saved input▌'))
  const second = props({ sessionId: 'session-second', committed: appendTranscript(emptyTranscript, [{ kind: 'user', text: 'Second saved input' }]) })
  ui.rerender(<App {...second} />)
  await vi.waitFor(() => expect(ui.lastFrame()).toContain('Session: session-second'))
  expect(ui.lastFrame()).not.toContain('First saved input▌')
  expect(ui.lastFrame()).not.toContain('Unsent draft')
  ui.stdin.write('\u001b[A')
  await vi.waitFor(() => expect(ui.lastFrame()).toContain('> Second saved input▌'))
  ui.stdin.write('\u001b[B')
  await vi.waitFor(() => expect(ui.lastFrame()).toMatch(/> ▌/))
  ui.stdin.write('Blocked draft')
  await vi.waitFor(() => expect(ui.lastFrame()).toContain('Blocked draft▌'))
  ui.rerender(<App {...second} inputBlocked />)
  ui.stdin.write('\u001b[200~paste\u001b[201~')
  ui.stdin.write('typed\r')
  ui.stdin.write('\u001b')
  await vi.waitFor(() => expect(second.onCancel).toHaveBeenCalledOnce())
  expect(second.onSubmit).not.toHaveBeenCalled()
  ui.rerender(<App {...second} />)
  await vi.waitFor(() => expect(ui.lastFrame()).toContain('Blocked draft▌'))
})

it.each(['en', 'zh'] as const)('renders staged and queued attachment metadata in %s', async locale => {
  const item = { name: 'pixel.png', bytes: 84, mediaType: 'image/png', width: 1, height: 1 }
  const state = props({ copy: dictionaries[locale], attachments: [item], pending: [{ id: 'queued', target: 'next-step', text: 'Inspect', attachments: [item] }] })
  const ui = render(<App {...state} />)
  await vi.waitFor(() => expect(ui.lastFrame()).toContain(state.copy.attachmentsTitle))
  await expect(ui.lastFrame() + '\n').toMatchFileSnapshot(`./expected/attachments.${locale}.txt`)
})

it('retains draft and cursor on refused admission and locks same-read edits until acceptance', async () => {
  const first = Promise.withResolvers<boolean>()
  const second = Promise.withResolvers<boolean>()
  const submit = vi.fn().mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise)
  const state = props({ onSubmit: submit, attachments: [{ name: 'data.bin', bytes: 4 }] })
  const ui = render(<App {...state} />)
  ui.stdin.write('hello\u001b[D\rignored\r')
  await vi.waitFor(() => expect(ui.lastFrame()).toContain(state.copy.attachmentsSending))
  expect(submit).toHaveBeenCalledExactlyOnceWith('hello')
  ui.stdin.write('\u001b[200~ignored paste\u001b[201~')
  ui.stdin.write('\u0010')
  ui.stdin.write('\u001b')
  await vi.waitFor(() => expect(state.onCancel).toHaveBeenCalledOnce())
  first.resolve(false)
  await vi.waitFor(() => expect(ui.lastFrame()).not.toContain(state.copy.attachmentsSending))
  expect(ui.lastFrame()).toContain('> hell▌o')
  ui.stdin.write('\r')
  await vi.waitFor(() => expect(submit).toHaveBeenCalledTimes(2))
  second.resolve(true)
  await vi.waitFor(() => expect(ui.lastFrame()).toContain('> ▌'))
  expect(ui.lastFrame()).not.toContain('hello')
})

it('allows attachment-only Enter and retains text after synchronous refusal or promise rejection', async () => {
  const submit = vi.fn().mockReturnValueOnce(true).mockReturnValueOnce(false).mockImplementationOnce(() => Promise.reject(new Error('Failed')))
  const ui = render(<App {...props({ onSubmit: submit, attachments: [{ name: 'data.bin', bytes: 4 }] })} />)
  ui.stdin.write('\r')
  await vi.waitFor(() => expect(submit).toHaveBeenCalledExactlyOnceWith(''))
  ui.stdin.write('/external\r')
  await vi.waitFor(() => expect(submit).toHaveBeenCalledTimes(2))
  expect(ui.lastFrame()).toContain('> /external▌')
  ui.stdin.write('\r')
  await vi.waitFor(() => expect(submit).toHaveBeenCalledTimes(3))
  await vi.waitFor(() => expect(ui.lastFrame()).not.toContain(dictionaries.en.attachmentsSending))
  expect(ui.lastFrame()).toContain('> /external▌')
})
