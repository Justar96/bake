/** Input integration against Ink's real key and paste channels. */
import React from 'react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { cleanup, render } from '../../../tests/render.tsx'
import { App, type AppProps } from '../src/app.tsx'
import { appendTranscript, emptyTranscript } from '../src/transcript.ts'
import { dictionaries } from '../src/copy.ts'



afterEach(cleanup)

function props(overrides: Partial<AppProps> = {}): AppProps {
  return {
    files: { query: undefined, entries: [], loading: false, error: undefined }, onReferenceQuery: () => {},
    completion: { entries: [], loading: false, error: undefined }, completionLimit: 8, resultLines: 8,
    committed: emptyTranscript, live: [], pending: [], status: 'idle', stopping: false,
    command: undefined, notice: undefined, interaction: undefined, todos: undefined,
    model: 'mock/model', cwd: '/workspace', sessionId: 'session-test', copy: dictionaries.en, frame: 'round', quitting: false, context: undefined,
    onSubmit: vi.fn(), onCancel: vi.fn(), onInterrupt: vi.fn(), onAnswer: vi.fn(), ...overrides,
  }
}

describe('terminal composer', () => {
  it('submits a slash command typed one key at a time with its full name', async () => {
    const state = props({ completion: { loading: false, error: undefined, entries: [
      { name: 'compact', description: 'Compact history', kind: 'command' },
    ] } })
    const ui = render(<App {...state} />)
    for (const [index, character] of [...'/compact'].entries()) {
      ui.stdin.write(character)
      await vi.waitFor(() => expect(ui.lastFrame()).toContain(`> ${'/compact'.slice(0, index + 1)}▌`))
    }
    ui.stdin.write('\r')
    await vi.waitFor(() => expect(state.onSubmit).toHaveBeenCalledExactlyOnceWith('/compact'))
  })

  it('runs the selected slash command on Enter instead of submitting the slash prefix', async () => {
    const state = props({ completion: { loading: false, error: undefined, entries: [
      { name: 'compact', description: 'Compact history', kind: 'command' },
    ] } })
    const ui = render(<App {...state} />)
    ui.stdin.write('/')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('▸ /compact'))
    ui.stdin.write('\r')
    await vi.waitFor(() => expect(state.onSubmit).toHaveBeenCalledExactlyOnceWith('/compact'))
  })

  it('fills a command that needs arguments on Enter and keeps its usage in view', async () => {
    const state = props({ completion: { loading: false, error: undefined, entries: [
      { name: 'attach', description: 'Stage a file', kind: 'command', hint: '<path>' },
    ] } })
    const ui = render(<App {...state} />)
    ui.stdin.write('/att')
    await vi.waitFor(() => expect(ui.lastFrame()).toMatch(/▸ \/attach +<path> {2}Stage a file/u))
    ui.stdin.write('\r')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('> /attach ▌'))
    expect(state.onSubmit).not.toHaveBeenCalled()
    expect(ui.lastFrame()).toContain('/attach <path>  Stage a file')
    ui.stdin.write('notes.md')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('> /attach notes.md▌'))
    expect(ui.lastFrame()).toContain('/attach <path>  Stage a file')
    ui.stdin.write('\r')
    await vi.waitFor(() => expect(state.onSubmit).toHaveBeenCalledExactlyOnceWith('/attach notes.md'))
  })

  it('inserts an argument choice, then submits an exact complete line', async () => {
    const onArgumentQuery = vi.fn()
    const state = props({ onArgumentQuery, completion: { loading: false, error: undefined, entries: [
      { name: 'goal', description: 'Set a goal', kind: 'command', hint: '[objective|clear]', choices: true },
    ] } })
    const ui = render(<App {...state} />)
    ui.stdin.write('/goal cl')
    await vi.waitFor(() => expect(onArgumentQuery).toHaveBeenCalledWith({ name: 'goal', partial: 'cl' }))
    ui.rerender(<App {...state} completion={{ ...state.completion,
      argument: { name: 'goal', partial: 'cl', entries: ['clear', 'edit'], loading: false, error: undefined },
    }} />)
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('▸ clear'))
    ui.stdin.write('\t')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('> /goal clear ▌'))
    expect(state.onSubmit).not.toHaveBeenCalled()
    ui.stdin.write('\r')
    await vi.waitFor(() => expect(state.onSubmit).toHaveBeenCalledExactlyOnceWith('/goal clear '))
  })

  it('shows usage when an argument has no matching choices', async () => {
    const state = props({ completion: { loading: false, error: undefined,
      entries: [{ name: 'plan', description: 'Plan mode', kind: 'command', hint: '[off|message]', choices: true }],
      argument: { name: 'plan', partial: 'hello', entries: ['off'], loading: false, error: undefined },
    } })
    const ui = render(<App {...state} />)
    ui.stdin.write('/plan hello')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('/plan [off|message]  Plan mode'))
    ui.stdin.write('\r')
    await vi.waitFor(() => expect(state.onSubmit).toHaveBeenCalledExactlyOnceWith('/plan hello'))
  })

  it('keeps an incomplete argument choice open for more text', async () => {
    const state = props({ completion: { loading: false, error: undefined,
      entries: [{ name: 'goal', description: 'Set a goal', kind: 'command', hint: '[objective|edit <objective>]', choices: true }],
      argument: { name: 'goal', partial: 'edit', entries: [{ value: 'edit', requiresInput: true }], loading: false, error: undefined },
    } })
    const ui = render(<App {...state} />)
    ui.stdin.write('/goal edit')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('▸ edit'))
    ui.stdin.write('\r')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('> /goal edit ▌'))
    expect(state.onSubmit).not.toHaveBeenCalled()
    ui.stdin.write('Update docs')
    ui.stdin.write('\r')
    await vi.waitFor(() => expect(state.onSubmit).toHaveBeenCalledExactlyOnceWith('/goal edit Update docs'))
  })

  it('runs a command with an optional argument on Enter before a choice is typed', async () => {
    const state = props({ completion: { loading: false, error: undefined,
      entries: [{ name: 'login', description: 'Sign in', kind: 'command', hint: '[target]', choices: true }],
      argument: { name: 'login', partial: '', entries: ['deepseek', 'openai'], loading: false, error: undefined },
    } })
    const ui = render(<App {...state} />)
    ui.stdin.write('/login ')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('▸ deepseek'))
    ui.stdin.write('\r')
    await vi.waitFor(() => expect(state.onSubmit).toHaveBeenCalledExactlyOnceWith('/login '))
  })

  it('submits an exact complete argument choice on Enter', async () => {
    const state = props({ completion: { loading: false, error: undefined,
      entries: [{ name: 'plan', description: 'Plan mode', kind: 'command', hint: '[off|message]', choices: true }],
      argument: { name: 'plan', partial: 'off', entries: ['off'], loading: false, error: undefined },
    } })
    const ui = render(<App {...state} />)
    ui.stdin.write('/plan off')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('▸ off'))
    ui.stdin.write('\r')
    await vi.waitFor(() => expect(state.onSubmit).toHaveBeenCalledExactlyOnceWith('/plan off'))
  })

  it('inserts a selected skill on Enter and submits it when its full name is typed', async () => {
    const state = props({ completion: { loading: false, error: undefined, entries: [
      { name: 'review', description: 'Review a patch', kind: 'skill' },
    ] } })
    const ui = render(<App {...state} />)
    ui.stdin.write('/rev')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('▸ /review'))
    ui.stdin.write('\r')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('> /review▌'))
    expect(state.onSubmit).not.toHaveBeenCalled()
    ui.stdin.write('\r')
    await vi.waitFor(() => expect(state.onSubmit).toHaveBeenCalledExactlyOnceWith('/review'))
  })

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
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('▸ /review'))
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
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('▸ /help'))
    ui.stdin.write('\u001b')
    // Wait on the menu's own rows. The panel has no title to disappear, and
    // waiting on something never rendered would pass before Esc was read.
    await vi.waitFor(() => expect(ui.lastFrame()).not.toContain('/help'))
    expect(state.onCancel).not.toHaveBeenCalled()
    ui.stdin.write('\u001b')
    await vi.waitFor(() => expect(state.onCancel).toHaveBeenCalledTimes(1))
    ui.stdin.write('he')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('▸ /help'))
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
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('▸ @"notes folder/'))
    expect(query).toHaveBeenLastCalledWith('notes')
    ui.stdin.write('\t')
    await vi.waitFor(() => expect(query).toHaveBeenLastCalledWith('notes folder/'))
    expect(ui.lastFrame()).toContain('> Review @"notes folder/▌')
    expect(state.onSubmit).not.toHaveBeenCalled()
    ui.rerender(<App {...state} files={{ query: 'notes folder/', loading: false, error: undefined, entries: [
      { path: 'notes folder/read me.txt', kind: 'file' }, { path: 'notes folder/todo.txt', kind: 'file' },
    ] }} />)
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('▸ @"notes folder/read me.txt"'))
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
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('▸ mock/current'))
    expect(ui.lastFrame()).not.toContain(state.copy.prompt)
    expect(ui.lastFrame()).not.toContain('provider/model')
    ui.stdin.write('\u001b[B')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('▸ mock/other'))
    await expect(ui.lastFrame() + '\n').toMatchFileSnapshot(`./expected/model-picker.${locale}.txt`)
    ui.stdin.write('\u001b[200~provider\u001b[201~')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('▸ provider/model'))
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
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('▸ High'))
    await expect(ui.lastFrame() + '\n').toMatchFileSnapshot(`./expected/effort-picker.${locale}.txt`)
    ui.stdin.write('\u001b[B')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain(`\u25b8 ${copy.providerDefault}`))
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
    // The composer's frame sits between the two draft rows in the frame text,
    // so the rows are read separately. Joining them would hide the frame between them.
    await vi.waitFor(() => {
      const rows = ui.lastFrame()!.split('\n')
      const first = rows.findIndex(row => row.includes('> first'))
      expect(first).toBeGreaterThanOrEqual(0)
      expect(rows[first + 1]).toContain('  ▌')
    })
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
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('▸ /help'))
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
    await vi.waitFor(() => expect(ui.lastFrame()).toContain(dictionaries.en.prompt))
    ui.stdin.write('/select  item \r')
    await vi.waitFor(() => expect(state.onSubmit).toHaveBeenCalledExactlyOnceWith('/select  item '))
  })

  it('keeps a fragmented multiline paste in the draft until Enter', async () => {
    const state = props()
    const ui = render(<App {...state} />)
    await vi.waitFor(() => expect(ui.lastFrame()).toContain(dictionaries.en.prompt))
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
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('▸ 2. Implement'))
    ui.stdin.write('\r')
    await vi.waitFor(() => expect(state.onAnswer).toHaveBeenCalledWith(3, { answers: [{ id: 'plan', selected: ['Implement'] }] }))
  })

  it('keeps the Other draft visible while navigating selectable answers', async () => {
    const state = props({ interaction: { id: 4, kind: 'questions', questions: [{
      id: 'choice', question: 'Choose an approach', options: [{ label: 'A' }, { label: 'B' }],
    }] } })
    const ui = render(<App {...state} />)
    ui.stdin.write('A different approach')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('Other answer: A different approach▌'))
    ui.stdin.write('\u001b[A')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('▸ 2. B'))
    expect(ui.lastFrame()).toContain('Other answer: A different approach')
    expect(ui.lastFrame()).not.toContain('Other answer: A different approach▌')
    ui.stdin.write('\u001b[B\r')
    await vi.waitFor(() => expect(state.onAnswer).toHaveBeenCalledWith(4, { answers: [
      { id: 'choice', selected: [], custom: 'A different approach' },
    ] }))
  })

  it('submits toggled multi-select answers together with persistent custom text', async () => {
    const state = props({ completionLimit: 2, interaction: { id: 5, kind: 'questions', questions: [{
      id: 'choices', question: 'Choose several', multiSelect: true,
      options: [{ label: 'A' }, { label: 'B' }, { label: 'C' }],
    }] } })
    const ui = render(<App {...state} />)
    ui.stdin.write(' ')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('[✓] 1. A'))
    ui.stdin.write('\u001b[B ')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('[✓] 2. B'))
    ui.stdin.write('Additional context')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('Other answer: Additional context▌'))
    ui.stdin.write('\u001b[A')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('▸ [ ] 3. C'))
    expect(ui.lastFrame()).toContain('Other answer: Additional context')
    ui.stdin.write('\r')
    await vi.waitFor(() => expect(state.onAnswer).toHaveBeenCalledWith(5, { answers: [
      { id: 'choices', selected: ['A', 'B'], custom: 'Additional context' },
    ] }))
  })

  it('keeps an empty Other answer open, accepts paste, and advances through questions', async () => {
    const state = props({ interaction: { id: 6, kind: 'questions', questions: [
      { id: 'first', question: 'Choose one', options: [{ label: 'A' }] },
      { id: 'second', question: 'Explain', options: [] },
    ] } })
    const ui = render(<App {...state} />)
    ui.stdin.write('\u001b[B\r')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('Other answer: ▌'))
    expect(state.onAnswer).not.toHaveBeenCalled()
    ui.stdin.write('\u001b[200~Custom first\u001b[201~\r')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('Explain'))
    expect(state.onAnswer).not.toHaveBeenCalled()
    ui.stdin.write('Custom second')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('Other answer: Custom second▌'))
    ui.stdin.write('\r')
    await vi.waitFor(() => expect(state.onAnswer).toHaveBeenCalledExactlyOnceWith(6, { answers: [
      { id: 'first', selected: [], custom: 'Custom first' },
      { id: 'second', selected: [], custom: 'Custom second' },
    ] }))
  })

  it('says why an empty Enter did nothing, reaches Other by its number, and ticks it once it has text', async () => {
    const copy = dictionaries.en
    const state = props({ interaction: { id: 8, kind: 'questions', questions: [
      { id: 'first', header: 'Checks', question: 'Which checks?', multiSelect: true,
        options: [{ label: 'Types', description: 'tsc -b' }, { label: 'Unit tests' }] },
      { id: 'second', question: 'Anything else?', options: [] },
    ] } })
    const ui = render(<App {...state} />)
    await vi.waitFor(() => expect(ui.lastFrame()).toMatch(/Answer required {2}Checks .*●○ {2}1\/2/))
    // Descriptions line up in a column after the longest described label.
    expect(ui.lastFrame()).toContain('[ ] 1. Types  tsc -b')
    ui.stdin.write('\r')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain(copy.questionChooseOne))
    expect(state.onAnswer).not.toHaveBeenCalled()
    ui.stdin.write('3')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain(`▸ [ ] 3. ${copy.customAnswer}: ▌${copy.customAnswerHint}`))
    expect(ui.lastFrame()).not.toContain(copy.questionChooseOne)
    ui.stdin.write('lint')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain(`▸ [✓] 3. ${copy.customAnswer}: lint▌`))
    expect(ui.lastFrame()).toContain(`1 ${copy.questionSelected}`)
    ui.stdin.write('\r')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('Anything else?'))
    // The finished step turns green and the next one is current.
    expect(ui.lastFrame()).toMatch(/●● {2}2\/2/)
    ui.stdin.write('\r')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain(copy.questionTypeFirst))
    expect(state.onAnswer).not.toHaveBeenCalled()
  })

  it('cancels a question without submitting its Other draft', async () => {
    const state = props({ interaction: { id: 7, kind: 'questions', questions: [
      { id: 'choice', question: 'Choose', options: [{ label: 'A' }] },
    ] } })
    const ui = render(<App {...state} />)
    ui.stdin.write('Unsent answer')
    await vi.waitFor(() => expect(ui.lastFrame()).toContain('Other answer: Unsent answer▌'))
    ui.stdin.write('\u001b')
    await vi.waitFor(() => expect(state.onCancel).toHaveBeenCalledOnce())
    expect(state.onAnswer).not.toHaveBeenCalled()
  })
})

it.each(['en', 'zh'] as const)('renders a bounded session picker and returns the selected id in %s', async locale => {
  const copy = dictionaries[locale]
  const state = props({ copy, inputBlocked: true, completionLimit: 2,
    interaction: { kind: 'select', id: 10, title: copy.chooseSession, initial: 'session-first', choices: [
      { value: 'session-first', label: 'First conversation', role: 'session-current', description: `5${copy.ageMinutes} · 514e6406` },
      { value: 'session-second', label: 'Second conversation', role: 'session-saved', description: `3${copy.ageDays} · a4182799` },
      { value: '', label: copy.newSession, role: 'session-new', pinned: true },
    ] },
  })
  const ui = render(<App {...state} />)
  await vi.waitFor(() => expect(ui.lastFrame()).toContain(copy.chooseSession))
  // Two rows. The scrolled list gives one up so the pinned action stays in view.
  expect(ui.lastFrame()).toContain(`+ ${copy.newSession}`)
  expect(ui.lastFrame()).not.toContain('Second conversation')
  await expect(ui.lastFrame() + '\n').toMatchFileSnapshot(`./expected/session-picker.${locale}.txt`)
  ui.stdin.write('\u001b[B')
  await vi.waitFor(() => expect(ui.lastFrame()).toContain('▸ ○ Second conversation'))
  expect(ui.lastFrame()).toContain(`+ ${copy.newSession}`)
  ui.stdin.write('\u001b[B')
  await vi.waitFor(() => expect(ui.lastFrame()).toContain(`▸ + ${copy.newSession}`))
  ui.stdin.write('\u001b[A')
  ui.stdin.write('\u001b[200~second\u001b[201~')
  await vi.waitFor(() => expect(ui.lastFrame()).toContain('▸ ○ Second conversation'))
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
