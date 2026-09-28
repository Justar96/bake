/** Detecting the terminal, finding its key bindings, and adding Shift+Enter without disturbing the rest. */
import { describe, expect, test } from 'bun:test'
import ts from 'typescript'
import { appendMember, parseJsonc, valueOf } from '../src/jsonc.ts'
import {
  configFiles, decodeEscapes, detectTerminal, editConfig, findBinding, MARK, NEWLINE_SEQUENCE, WT_ACTION_ID, type Edit,
} from '../src/terminal-bindings.ts'

/**
 * Parse JSONC with TypeScript's reader, which shares nothing with the module
 * under test. It takes an object at the root, so the document is wrapped in one.
 */
function jsonc(text: string): unknown {
  const parsed = ts.parseConfigFileTextToJson('settings.json', `{ "root":\n${text}\n}`)
  if (parsed.error !== undefined) throw new Error(ts.flattenDiagnosticMessageText(parsed.error.messageText, '\n'))
  return (parsed.config as { root: unknown }).root
}

function written(edit: Edit): string {
  if (edit.kind !== 'write') throw new Error(`expected a write, got ${JSON.stringify(edit)}`)
  return edit.text
}

const BINDING = { key: 'shift+enter', command: 'workbench.action.terminal.sendSequence', args: { text: '\x1b\r' }, when: 'terminalFocus' }

describe('VS Code keybindings.json', () => {
  test('creates the file with the one binding', () => {
    const edit = editConfig('keybindings', undefined)
    const text = written(edit)
    expect(jsonc(text)).toEqual([BINDING])
    // `sendSequence` reads \u escapes only, never \x.
    expect(text).toContain('"text": "\\u001b\\r"')
    expect(text).toContain(`// ${MARK}`)
    expect(edit.kind === 'write' && edit.snippet).toBe([
      `// ${MARK}`, '{', '    "key": "shift+enter",', '    "command": "workbench.action.terminal.sendSequence",',
      '    "args": {', '        "text": "\\u001b\\r"', '    },', '    "when": "terminalFocus"', '}',
    ].join('\n'))
  })

  test('appends to a file with comments, keeping every byte of it', () => {
    const original = [
      '// Place your key bindings in this file to override the defaults',
      '[',
      '  /* a block comment, with a "quote" and a ] bracket */',
      '  {',
      '    "key": "ctrl+k", // why: "clear" is muscle memory',
      '    "command": "workbench.action.terminal.clear",',
      '    "when": "terminalFocus"',
      '  } // trailing note',
      '  // before the end',
      ']',
      '',
    ].join('\n')
    const text = written(editConfig('keybindings', original))
    expect(jsonc(text)).toEqual([{ key: 'ctrl+k', command: 'workbench.action.terminal.clear', when: 'terminalFocus' }, BINDING])
    // The only changes: a comma after the last binding, and the new lines above the bracket, at its indentation.
    expect(text.replace(`,${' // trailing note'}`, ' // trailing note').replace(
      `  // ${MARK}\n  {\n    "key": "shift+enter",\n    "command": "workbench.action.terminal.sendSequence",\n`
      + '    "args": {\n      "text": "\\u001b\\r"\n    },\n    "when": "terminalFocus"\n  }\n', '')).toBe(original)
  })

  test('keeps a trailing comma, CRLF line endings, and a one-line empty array', () => {
    const trailing = written(editConfig('keybindings', '[\r\n    { "key": "f5", "command": "a" },\r\n]\r\n'))
    expect(jsonc(trailing)).toEqual([{ key: 'f5', command: 'a' }, BINDING])
    expect(trailing).not.toContain(',,')
    expect(trailing.replaceAll('\r\n', '')).not.toContain('\n')
    expect(jsonc(written(editConfig('keybindings', '[]')))).toEqual([BINDING])
    expect(jsonc(written(editConfig('keybindings', '// only a comment\n')))).toEqual([BINDING])
  })

  test('a second run finds the binding and changes nothing', () => {
    const once = written(editConfig('keybindings', '[\n    { "key": "f5", "command": "a" }\n]\n'))
    expect(editConfig('keybindings', once)).toEqual({ kind: 'present' })
    expect(findBinding('keybindings', once)).toEqual({ kind: 'present' })
  })

  test('reports a different Shift+Enter terminal binding instead of overwriting it', () => {
    const other = '[\n  { "key": "shift+enter", "command": "workbench.action.terminal.sendSequence", "args": { "text": "\\\\\\r\\n" },\n'
      + '    "when": "terminalFocus" }\n]\n'
    const edit = editConfig('keybindings', other)
    expect(edit.kind).toBe('conflict')
    expect(edit.kind === 'conflict' && edit.existing).toContain('"text": "\\\\\\r\\n"')
    // The last matching rule wins, so an older ESC CR rule below it does not count.
    const shadowed = `[\n  ${JSON.stringify(BINDING)},\n  { "key": "Shift+Enter", "command": "x" }\n]`
    expect(editConfig('keybindings', shadowed).kind).toBe('conflict')
  })

  test('ignores Shift+Enter rules that cannot fire in the terminal', () => {
    const editor = '[{ "key": "shift+enter", "command": "editor.action.insertLineAfter", "when": "editorTextFocus" },'
      + ' { "key": "shift+enter", "command": "-workbench.action.terminal.sendSequence" },'
      + ' { "key": "shift+enter", "command": "x", "when": "!terminalFocus" }]'
    expect(editConfig('keybindings', editor).kind).toBe('write')
  })

  test('leaves a file it cannot parse to the user', () => {
    expect(editConfig('keybindings', '[ { "key": ')).toEqual({ kind: 'manual', reason: 'unreadable' })
    expect(editConfig('keybindings', '{ "key": "shift+enter" }')).toEqual({ kind: 'manual', reason: 'unreadable' })
  })
})

describe('Windows Terminal settings.json', () => {
  const SEND = { action: 'sendInput', input: '\x1b\r' }

  test('adds an action with inline keys to a file without the split layout', () => {
    const original = '// This file was initially generated by Windows Terminal\n{\n    "defaultProfile": "{x}",\n'
      + '    "actions": [\n        { "command": "paste", "keys": "ctrl+v" }\n    ]\n}\n'
    const text = written(editConfig('windows-terminal', original))
    expect((jsonc(text) as { actions: unknown[] }).actions)
      .toEqual([{ command: 'paste', keys: 'ctrl+v' }, { command: SEND, keys: 'shift+enter' }])
    expect(text).toStartWith('// This file was initially generated by Windows Terminal\n')
    expect(editConfig('windows-terminal', text)).toEqual({ kind: 'present' })
  })

  test('adds an action and a keybinding by id in the 1.21 layout', () => {
    const original = '{\n    "actions": [\n        { "command": "paste", "id": "User.paste" }\n    ],\n'
      + '    "keybindings": [\n        { "id": "User.paste", "keys": "ctrl+v" }\n    ]\n}\n'
    const text = written(editConfig('windows-terminal', original))
    const settings = jsonc(text) as { actions: unknown[]; keybindings: unknown[] }
    expect(settings.actions.at(-1)).toEqual({ command: SEND, id: WT_ACTION_ID })
    expect(settings.keybindings.at(-1)).toEqual({ id: WT_ACTION_ID, keys: 'shift+enter' })
    expect(editConfig('windows-terminal', text)).toEqual({ kind: 'present' })
  })

  test('creates the actions array when the file has none', () => {
    const text = written(editConfig('windows-terminal', '{\n  "theme": "dark" // mine\n}\n'))
    expect(jsonc(text)).toEqual({ theme: 'dark', actions: [{ command: SEND, keys: 'shift+enter' }] })
    expect(text).toContain('"theme": "dark", // mine')
  })

  test('reports Shift+Enter bound or unbound elsewhere', () => {
    const bound = '{ "actions": [ { "command": "copy", "id": "User.copy" } ],'
      + ' "keybindings": [ { "id": "User.copy", "keys": "shift+enter" } ] }'
    expect(editConfig('windows-terminal', bound).kind).toBe('conflict')
    expect(editConfig('windows-terminal', '{ "keybindings": [ { "id": null, "keys": ["shift+enter"] } ] }').kind).toBe('conflict')
    const other = '{ "actions": [ { "command": { "action": "sendInput", "input": "\\n" }, "keys": "shift+enter" } ] }'
    expect(editConfig('windows-terminal', other).kind).toBe('conflict')
  })

  test('never creates a settings file', () => {
    expect(editConfig('windows-terminal', undefined)).toEqual({ kind: 'manual', reason: 'unreadable' })
  })
})

describe('Ghostty config', () => {
  test('appends the keybind to an empty or commented file', () => {
    expect(written(editConfig('ghostty', undefined))).toBe(`# ${MARK}\nkeybind = shift+enter=text:\\x1b\\r\n`)
    const original = '# my theme\ntheme = dark\n'
    expect(written(editConfig('ghostty', original))).toBe(`${original}\n# ${MARK}\nkeybind = shift+enter=text:\\x1b\\r\n`)
  })

  test('is idempotent and reports another Shift+Enter keybind', () => {
    expect(editConfig('ghostty', written(editConfig('ghostty', 'font-size = 12')))).toEqual({ kind: 'present' })
    expect(editConfig('ghostty', 'keybind = global:shift+enter=text:\\x1b\\r')).toEqual({ kind: 'present' })
    expect(editConfig('ghostty', 'keybind = shift+enter=text:\\n'))
      .toEqual({ kind: 'conflict', existing: 'keybind = shift+enter=text:\\n' })
    // `clear` drops every keybind before it.
    expect(editConfig('ghostty', 'keybind = shift+enter=unbind\nkeybind = clear\n').kind).toBe('write')
    expect(editConfig('ghostty', 'keybind = ctrl+enter=text:\\n\nkeybind = shift+a=text:+=').kind).toBe('write')
  })
})

describe('kitty.conf', () => {
  test('appends the mapping and finds it again', () => {
    const text = written(editConfig('kitty', '# BEGIN_KITTY_THEME\ninclude theme.conf\n'))
    expect(text).toEndWith(`\n\n# ${MARK}\nmap shift+enter send_text all \\x1b\\r\n`)
    expect(editConfig('kitty', text)).toEqual({ kind: 'present' })
    expect(editConfig('kitty', 'map shift+enter send_text normal,application \\e\\r')).toEqual({ kind: 'present' })
  })

  test('reports another Shift+Enter mapping and ignores conditional ones', () => {
    expect(editConfig('kitty', 'map shift+enter send_text all \\n').kind).toBe('conflict')
    expect(editConfig('kitty', 'map shift+return new_window').kind).toBe('conflict')
    expect(editConfig('kitty', 'map --when-focus-on var:vim shift+enter send_text all x').kind).toBe('write')
    expect(editConfig('kitty', 'map ctrl+x>shift+enter send_text all x').kind).toBe('write')
  })
})

describe('alacritty.toml', () => {
  test('appends a [[keyboard.bindings]] table that TOML reads as ESC CR', () => {
    const original = '# colours\n[colors.primary]\nbackground = "#000000"\n\n'
      + '[[keyboard.bindings]]\nkey = "N"\nmods = "Control|Shift"\naction = "CreateNewWindow"\n'
    const text = written(editConfig('alacritty', original))
    expect(text).toStartWith(original)
    const parsed = Bun.TOML.parse(text) as { keyboard: { bindings: Record<string, string>[] } }
    expect(parsed.keyboard.bindings.at(-1)).toEqual({ key: 'Enter', mods: 'Shift', chars: NEWLINE_SEQUENCE })
    expect(editConfig('alacritty', text)).toEqual({ kind: 'present' })
    expect(Bun.TOML.parse(written(editConfig('alacritty', undefined))))
      .toEqual({ keyboard: { bindings: [{ key: 'Enter', mods: 'Shift', chars: NEWLINE_SEQUENCE }] } })
  })

  test('reports any other Shift+Enter binding, since every binding of a key fires', () => {
    expect(editConfig('alacritty', '[[keyboard.bindings]]\nkey = "Return"\nmods = "Shift"\nchars = "\\n"\n').kind).toBe('conflict')
  })

  test('leaves an inline bindings array to the user, but still finds the binding in it', () => {
    expect(editConfig('alacritty', '[keyboard]\nbindings = [\n  { key = "N", mods = "Control|Shift", action = "CreateNewWindow" },\n]\n'))
      .toEqual({ kind: 'manual', reason: 'inline' })
    const inline = (entry: string): string => `[keyboard]\nbindings = [ { key = "Enter", mods = "Shift", ${entry} } ]\n`
    expect(editConfig('alacritty', inline('chars = "\\u001b\\r"'))).toEqual({ kind: 'present' })
    expect(editConfig('alacritty', inline('action = "ReceiveChar"')).kind).toBe('conflict')
  })
})

describe('decodeEscapes', () => {
  test('reads the escapes Zig, ANSI C, and TOML share', () => {
    expect(decodeEscapes('\\x1b\\r')).toBe('\x1b\r')
    expect(decodeEscapes('\\e\\r')).toBe('\x1b\r')
    expect(decodeEscapes('\\u001b\\r')).toBe('\x1b\r')
    expect(decodeEscapes('\\u{1b}\\U0000000d')).toBe('\x1b\r')
  })
})

describe('detectTerminal', () => {
  test('names VS Code and the editors built on it', () => {
    expect(detectTerminal({ TERM_PROGRAM: 'vscode' })).toEqual({ terminal: 'vscode', tmux: false, remote: false })
    expect(detectTerminal({ TERM_PROGRAM: 'vscode', CURSOR_TRACE_ID: 'x' }).terminal).toBe('cursor')
    const windsurf = '/Applications/Windsurf.app/Contents/Resources/app/extensions/git/dist/askpass-main.js'
    expect(detectTerminal({ TERM_PROGRAM: 'vscode', VSCODE_GIT_ASKPASS_MAIN: windsurf }).terminal).toBe('windsurf')
    expect(detectTerminal({ TERM_PROGRAM: 'vscode', TERM_PROGRAM_VERSION: '1.96.0-insider' }).terminal).toBe('vscode-insiders')
  })

  test('prefers the terminal’s own TERM over an inherited TERM_PROGRAM', () => {
    expect(detectTerminal({ TERM: 'xterm-kitty', TERM_PROGRAM: 'vscode', KITTY_WINDOW_ID: '1' }).terminal).toBe('kitty')
    expect(detectTerminal({ TERM: 'xterm-ghostty' }).terminal).toBe('ghostty')
    expect(detectTerminal({ WT_SESSION: 'guid' }).terminal).toBe('windows-terminal')
    expect(detectTerminal({ TERM_PROGRAM: 'Apple_Terminal' }).terminal).toBe('apple-terminal')
    expect(detectTerminal({ TERM: 'xterm-256color' }).terminal).toBeUndefined()
  })

  test('inside tmux, identifies the terminal around it from what it left in the environment', () => {
    expect(detectTerminal({ TMUX: '/tmp/tmux-1/default,1,0', TERM: 'tmux-256color', TERM_PROGRAM: 'tmux', ALACRITTY_WINDOW_ID: '9' }))
      .toEqual({ terminal: 'alacritty', tmux: true, remote: false })
    expect(detectTerminal({ TMUX: 'x', TERM: 'screen', TERM_PROGRAM: 'tmux' })).toEqual({ terminal: undefined, tmux: true, remote: false })
  })

  test('marks SSH and remote editor windows, whose terminal files are on another machine', () => {
    expect(detectTerminal({ SSH_CONNECTION: '1 2 3 4', TERM: 'xterm-kitty' })).toEqual({ terminal: 'kitty', tmux: false, remote: true })
    const server = '/home/me/.vscode-server/bin/abc/extensions/git/dist/askpass-main.js'
    expect(detectTerminal({ TERM_PROGRAM: 'vscode', VSCODE_GIT_ASKPASS_MAIN: server }).remote).toBe(true)
  })
})

describe('configFiles', () => {
  const place = (platform: NodeJS.Platform, env: Record<string, string> = {}) =>
    ({ env, platform, home: platform === 'win32' ? 'C:\\Users\\me' : '/home/me' })

  test('puts editor keybindings in each platform’s application data', () => {
    expect(configFiles('vscode', place('linux'))?.files).toEqual(['/home/me/.config/Code/User/keybindings.json'])
    expect(configFiles('cursor', place('linux', { XDG_CONFIG_HOME: '/x' }))?.files).toEqual(['/x/Cursor/User/keybindings.json'])
    expect(configFiles('windsurf', { ...place('darwin'), home: '/Users/me' })?.files)
      .toEqual(['/Users/me/Library/Application Support/Windsurf/User/keybindings.json'])
    expect(configFiles('vscode', place('win32', { APPDATA: 'C:\\Users\\me\\AppData\\Roaming' }))?.files)
      .toEqual(['C:\\Users\\me\\AppData\\Roaming\\Code\\User\\keybindings.json'])
  })

  test('lists Windows Terminal’s packaged and unpackaged settings, only on Windows', () => {
    expect(configFiles('windows-terminal', place('win32', { LOCALAPPDATA: 'C:\\L' }))?.files).toEqual([
      'C:\\L\\Packages\\Microsoft.WindowsTerminal_8wekyb3d8bbwe\\LocalState\\settings.json',
      'C:\\L\\Packages\\Microsoft.WindowsTerminalPreview_8wekyb3d8bbwe\\LocalState\\settings.json',
      'C:\\L\\Microsoft\\Windows Terminal\\settings.json',
    ])
    expect(configFiles('windows-terminal', place('linux'))).toBeUndefined()
  })

  test('reads Ghostty’s layered files in load order and names a new one by version', () => {
    const version = (number: string) => ({ TERM_PROGRAM: 'ghostty', TERM_PROGRAM_VERSION: number })
    const mac = configFiles('ghostty', { ...place('darwin', version('1.2.3')), home: '/Users/me' })
    expect(mac).toMatchObject({ layered: true, create: '/Users/me/.config/ghostty/config.ghostty', files: [
      '/Users/me/.config/ghostty/config.ghostty', '/Users/me/.config/ghostty/config',
      '/Users/me/Library/Application Support/com.mitchellh.ghostty/config.ghostty',
      '/Users/me/Library/Application Support/com.mitchellh.ghostty/config',
    ] })
    expect(configFiles('ghostty', place('linux', version('1.1.3')))?.create).toBe('/home/me/.config/ghostty/config')
  })

  test('follows kitty’s and Alacritty’s search orders', () => {
    expect(configFiles('kitty', place('linux', { KITTY_CONFIG_DIRECTORY: '/k' }))?.files).toEqual(['/k/kitty.conf'])
    expect(configFiles('alacritty', place('linux', { XDG_CONFIG_HOME: '/x' }))).toMatchObject({
      files: ['/x/alacritty/alacritty.toml', '/x/alacritty.toml', '/home/me/.config/alacritty/alacritty.toml', '/home/me/.alacritty.toml'],
      create: '/x/alacritty/alacritty.toml',
    })
    expect(configFiles('wezterm', place('linux'))).toBeUndefined()
  })
})

describe('jsonc', () => {
  test('keeps offsets, reads comment markers inside strings as text, and adds a member', () => {
    const text = '{ "a": "// not a comment", /* c */ "b": [1, 2,], }'
    const root = parseJsonc(text)!
    expect(valueOf(root)).toEqual({ a: '// not a comment', b: [1, 2] })
    expect(root.kind === 'object' && text.slice(root.members[1]!.value.start, root.members[1]!.value.end)).toBe('[1, 2,]')
    expect(jsonc(appendMember(text, root as Extract<typeof root, { kind: 'object' }>, 'c', () => 'true', { unit: '  ', eol: '\n' })))
      .toEqual({ a: '// not a comment', b: [1, 2], c: true })
    expect(() => parseJsonc('{ "a": 1 } x')).toThrow(SyntaxError)
  })
})
