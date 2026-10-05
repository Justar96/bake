/**
 * `/terminal-setup` through the real command registry and interaction queue,
 * against a private home: the question shows the file and the lines, an
 * answer writes them after a backup, a second run changes nothing, and
 * cancelling leaves every file as it was.
 */
import { mkdir, mkdtemp, readdir, readFile, realpath, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { formatRow, transcriptRows } from 'bake-tui-ui'
import { dictionaries } from 'bake-tui-ui/copy.ts'
import { openSession } from '../src/session.ts'
import { SessionController } from '../src/controller.ts'
import type { TerminalHost } from '../src/terminal-setup.ts'
import { MARK } from '../src/terminal-bindings.ts'
import { harness } from './harness.ts'

const copy = dictionaries.en
const NOW = 1_760_000_000_000
const cleanup: (() => Promise<void>)[] = []
afterEach(async () => { for (const dispose of cleanup.splice(0).reverse()) await dispose() })

/** A controller whose `/terminal-setup` sees only `env` and a home it owns. */
async function connected(env: Record<string, string>) {
  // Resolved, as the command resolves the file it backs up: macOS's temporary
  // directory sits behind the /var -> /private/var symlink.
  const home = await realpath(await mkdtemp(join(tmpdir(), 'bake-terminal-setup-')))
  cleanup.push(() => rm(home, { recursive: true, force: true }))
  const fixture = await harness()
  cleanup.push(fixture.dispose)
  const terminal: TerminalHost = { env, platform: 'linux', home, now: () => NOW }
  let controller!: SessionController
  const handle = await openSession(fixture.ctx, {}, new AbortController().signal, (agent, selection) => {
    controller = new SessionController(fixture.ctx, agent, copy, { refs: [] }, () => {},
      { attachmentMaxBytes: 1048576, attachmentLimit: 8, terminal }, selection)
  })
  cleanup.push(async () => { controller.close(); await controller.drain(); await handle.dispose() })
  await controller.replay(new AbortController().signal)
  /** Run the command and wait for its question. */
  const ask = async () => {
    expect(controller.submit('/terminal-setup')).toBe(true)
    await vi.waitFor(() => expect(controller.view.interaction?.kind).toBe('select'))
    const question = controller.view.interaction!
    if (question.kind !== 'select') throw new Error('expected the confirmation')
    return question
  }
  /** Run the command to the end without a question. */
  const run = async () => {
    expect(controller.submit('/terminal-setup')).toBe(true)
    await controller.drain()
    expect(controller.view.interaction).toBeUndefined()
    return last()
  }
  const last = () => formatRow(transcriptRows(controller.view.committed).at(-1)!)
  return { home, controller, ask, run, last }
}

describe('/terminal-setup', () => {
  it('adds the VS Code binding after a backup, keeping the file’s comments, and changes nothing the second time', async () => {
    const { home, controller, ask, run, last } = await connected({ TERM_PROGRAM: 'vscode' })
    const file = join(home, '.config', 'Code', 'User', 'keybindings.json')
    const original = '// Place your key bindings in this file to override the defaults\n[\n    // mine\n'
      + '    { "key": "ctrl+k", "command": "workbench.action.terminal.clear" }\n]\n'
    await mkdir(join(home, '.config', 'Code', 'User'), { recursive: true })
    await writeFile(file, original)

    const question = await ask()
    expect(question.title).toBe(`${copy.terminalSetupTitle} · VS Code`)
    expect(question.warning).toContain(`${copy.terminalSetupAdds} ~/.config/Code/User/keybindings.json`)
    expect(question.warning).toContain('"command": "workbench.action.terminal.sendSequence"')
    expect(question.choices.map(choice => choice.value)).toEqual(['write', 'cancel'])
    // Nothing is written while the question is open.
    expect(await readFile(file, 'utf8')).toBe(original)
    controller.interactions.answer(question.id, 'write')
    await controller.drain()

    const text = await readFile(file, 'utf8')
    expect(text.startsWith('// Place your key bindings in this file to override the defaults\n[\n    // mine\n')).toBe(true)
    expect(text).toContain(`    // ${MARK}\n    {\n        "key": "shift+enter",`)
    expect(text).toContain('"text": "\\u001b\\r"')
    const backup = `${file}.bak-${NOW}`
    expect(await readFile(backup, 'utf8')).toBe(original)
    const report = last()
    expect(report).toContain(`VS Code · ${copy.terminalSetupDone}`)
    expect(report).toContain(`${copy.terminalSetupBackup} ~/.config/Code/User/keybindings.json.bak-${NOW}`)
    expect(report).toContain(`${copy.terminalSetupUndo} cp '${backup}' '${file}'`)
    expect(report).toContain(`VS Code ${copy.terminalSetupLive}`)
    expect(report).toContain(copy.terminalSetupTest)

    expect(await run()).toContain(`VS Code · ${copy.terminalSetupPresent}`)
    expect(await readFile(file, 'utf8')).toBe(text)
    expect((await readdir(join(home, '.config', 'Code', 'User'))).sort()).toEqual(['keybindings.json', `keybindings.json.bak-${NOW}`])
  })

  it('writes nothing when the question is cancelled', async () => {
    const { home, controller, ask, last } = await connected({ TERM_PROGRAM: 'vscode', CURSOR_TRACE_ID: 'trace' })
    const question = await ask()
    expect(question.title).toBe(`${copy.terminalSetupTitle} · Cursor`)
    expect(question.warning).toContain(`${copy.terminalSetupCreates} ~/.config/Cursor/User/keybindings.json`)
    controller.cancel()
    await controller.drain()
    expect(last()).toContain(copy.terminalSetupCancelled)
    await expect(readdir(join(home, '.config'))).rejects.toThrow()
  })

  it('creates Ghostty’s config, reports it on a second run, and leaves a cancelled edit untouched', async () => {
    const { home, controller, ask, run, last } = await connected({ TERM_PROGRAM: 'ghostty', TERM_PROGRAM_VERSION: '1.2.3' })
    const file = join(home, '.config', 'ghostty', 'config.ghostty')

    const cancelled = await ask()
    expect(cancelled.warning).toContain(`${copy.terminalSetupCreates} ~/.config/ghostty/config.ghostty`)
    expect(cancelled.warning).toContain('keybind = shift+enter=text:\\x1b\\r')
    controller.interactions.answer(cancelled.id, 'cancel')
    await controller.drain()
    expect(last()).toContain(copy.terminalSetupCancelled)
    await expect(readFile(file, 'utf8')).rejects.toThrow()

    const question = await ask()
    controller.interactions.answer(question.id, 'write')
    await controller.drain()
    expect(await readFile(file, 'utf8')).toBe(`# ${MARK}\nkeybind = shift+enter=text:\\x1b\\r\n`)
    const report = last()
    expect(report).toContain(`Ghostty · ${copy.terminalSetupDone}`)
    expect(report).toContain(`${copy.terminalSetupCreated} ~/.config/ghostty/config.ghostty`)
    expect(report).toContain(`${copy.terminalSetupUndo} rm '${file}'`)
    expect(report).toContain(copy.terminalSetupReloadGhostty)
    expect(report).not.toContain(copy.terminalSetupBackup)

    expect(await run()).toContain(`Ghostty · ${copy.terminalSetupPresent}`)
    expect(await readdir(join(home, '.config', 'ghostty'))).toEqual(['config.ghostty'])
  })

  it('edits the Ghostty file loaded last, after a backup, and inside tmux names the terminal around it', async () => {
    const { home, controller, ask } = await connected({
      TMUX: '/tmp/tmux-0/default,1,0', TERM_PROGRAM: 'tmux', GHOSTTY_RESOURCES_DIR: '/usr/share/ghostty',
    })
    const dir = join(home, '.config', 'ghostty')
    await mkdir(dir, { recursive: true })
    await writeFile(join(dir, 'config.ghostty'), 'theme = dark\n')
    await writeFile(join(dir, 'config'), 'font-size = 13\n')
    const question = await ask()
    expect(question.warning).toContain(copy.terminalSetupTmux)
    expect(question.warning).toContain(`${copy.terminalSetupAdds} ~/.config/ghostty/config`)
    controller.interactions.answer(question.id, 'write')
    await controller.drain()
    expect(await readFile(join(dir, 'config'), 'utf8')).toBe(`font-size = 13\n\n# ${MARK}\nkeybind = shift+enter=text:\\x1b\\r\n`)
    expect(await readFile(join(dir, `config.bak-${NOW}`), 'utf8')).toBe('font-size = 13\n')
    expect(await readFile(join(dir, 'config.ghostty'), 'utf8')).toBe('theme = dark\n')
  })

  it('reports a different Shift+Enter binding and leaves it', async () => {
    const { home, run } = await connected({ TERM: 'xterm-kitty', KITTY_WINDOW_ID: '1' })
    const file = join(home, '.config', 'kitty', 'kitty.conf')
    await mkdir(join(home, '.config', 'kitty'), { recursive: true })
    await writeFile(file, 'map shift+enter send_text all \\n\n')
    const report = await run()
    expect(report).toContain(`kitty · ${copy.terminalSetupConflict}`)
    expect(report).toContain('map shift+enter send_text all \\n')
    expect(report).toContain('map shift+enter send_text all \\x1b\\r')
    expect(await readFile(file, 'utf8')).toBe('map shift+enter send_text all \\n\n')
  })

  it('gives steps instead of an edit for WezTerm, an unknown terminal, and a remote session', async () => {
    const wezterm = await connected({ TERM_PROGRAM: 'WezTerm' })
    expect(await wezterm.run()).toContain('wezterm.action.SendString \'\\x1b\\r\'')
    const unknown = await connected({ TERM: 'xterm-256color' })
    expect(await unknown.run()).toContain(copy.terminalSetupUnknown)
    const remote = await connected({ TERM_PROGRAM: 'vscode', SSH_CONNECTION: '10.0.0.1 22 10.0.0.2 22' })
    const report = await remote.run()
    expect(report).toContain(`VS Code · ${copy.terminalSetupRemote}`)
    expect(report).toContain(copy.terminalSetupEditorSteps)
    await expect(readdir(join(remote.home, '.config'))).rejects.toThrow()
  })
})
