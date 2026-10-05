/**
 * Which terminal Bake runs in, where that terminal keeps its key bindings,
 * and the edit that makes Shift+Enter send ESC CR there. Pure: the caller
 * reads the environment and the files, and writes the result.
 *
 * Bake reads ESC CR, what Alt+Enter sends, as "insert a new line"
 * (`isNewline` in `@dsh-tui/ui/composer.ts`). Most terminals send Shift+Enter
 * as a bare CR, the same byte as Enter, and Bake cannot switch on the kitty
 * keyboard protocol that would tell them apart, so the fix is in the
 * terminal: bind Shift+Enter to send ESC CR.
 *
 * Every edit is additive. A file that already binds Shift+Enter to ESC CR is
 * left alone, and one that binds it to anything else is reported rather than
 * overwritten. A file whose syntax this module cannot follow safely is left
 * for the user, with the lines to add.
 *
 * @module @dsh-tui/app/terminal-bindings
 */
import { posix, win32 } from 'node:path'
import {
  appendItem, appendMember, blockJson, inlineJson, layoutOf, member, parseJsonc, valueOf, type JsonNode, type Layout,
} from './jsonc.ts'
import { isRecord } from 'bake-util-values'

/** What Shift+Enter should send: ESC CR, which Bake reads as Alt+Enter. */
export const NEWLINE_SEQUENCE = '\x1b\r'

/** The comment an edit leaves above the lines it adds, so the user can tell where they came from. */
export const MARK = 'Added by Bake /terminal-setup: Shift+Enter sends ESC CR, a new line in Bake'

/** Terminals `/terminal-setup` can name. */
export type TerminalId = 'vscode' | 'vscode-insiders' | 'vscodium' | 'cursor' | 'windsurf' | 'windows-terminal'
  | 'ghostty' | 'kitty' | 'alacritty' | 'wezterm' | 'iterm2' | 'apple-terminal'

/** Product names, as the terminals spell them. */
export const TERMINAL_NAMES: Readonly<Record<TerminalId, string>> = {
  'vscode': 'VS Code', 'vscode-insiders': 'VS Code Insiders', 'vscodium': 'VSCodium', 'cursor': 'Cursor', 'windsurf': 'Windsurf',
  'windows-terminal': 'Windows Terminal', 'ghostty': 'Ghostty', 'kitty': 'kitty', 'alacritty': 'Alacritty', 'wezterm': 'WezTerm',
  'iterm2': 'iTerm2', 'apple-terminal': 'Apple Terminal',
}

/** VS Code and the editors built on it share its keybindings format. */
export const EDITORS: ReadonlySet<TerminalId> = new Set(['vscode', 'vscode-insiders', 'vscodium', 'cursor', 'windsurf'])

/** Environment variables, read and never written. */
export type Env = Readonly<Record<string, string | undefined>>

/** Where Bake runs, as far as the environment says. */
export interface Detection {
  /** The terminal the user types into; undefined when nothing identifies it. */
  readonly terminal: TerminalId | undefined
  /** Bake runs inside tmux, so the terminal is the one around it. */
  readonly tmux: boolean
  /** Bake runs on another machine (SSH, or a remote editor window), so the terminal's files are not here. */
  readonly remote: boolean
}

const set = (env: Env, name: string): boolean => (env[name] ?? '') !== ''

/**
 * Name the terminal from its environment.
 *
 * `TERM` is set by the terminal itself, so outside tmux it wins over the
 * variables a terminal inherits from the one that launched it. tmux 3.2 and
 * later set `TERM` and `TERM_PROGRAM` to its own values
 * (https://github.com/tmux/tmux/blob/master/CHANGES, "Export TERM_PROGRAM and
 * TERM_PROGRAM_VERSION"), so inside it the markers the outer terminal left in
 * the environment identify it instead.
 * @param env - the process environment.
 * @returns the terminal, and whether Bake runs inside tmux or on a remote machine.
 */
export function detectTerminal(env: Env): Detection {
  const program = env['TERM_PROGRAM'] ?? ''
  const tmux = set(env, 'TMUX') || program === 'tmux'
  const remote = set(env, 'SSH_CONNECTION') || set(env, 'SSH_CLIENT') || set(env, 'SSH_TTY') || remoteEditor(env)
  const term = tmux ? '' : env['TERM'] ?? ''
  const byTerm: TerminalId | undefined = term === 'xterm-kitty' ? 'kitty' : term === 'xterm-ghostty' ? 'ghostty'
    : term === 'alacritty' ? 'alacritty' : term === 'wezterm' ? 'wezterm' : undefined
  const byProgram: TerminalId | undefined = program === 'vscode' ? editor(env) : program === 'iTerm.app' ? 'iterm2'
    : program === 'Apple_Terminal' ? 'apple-terminal' : program === 'WezTerm' ? 'wezterm' : program === 'ghostty' ? 'ghostty' : undefined
  const byMarker: TerminalId | undefined = set(env, 'WT_SESSION') ? 'windows-terminal'
    : set(env, 'KITTY_WINDOW_ID') ? 'kitty'
    : set(env, 'GHOSTTY_RESOURCES_DIR') ? 'ghostty'
    : set(env, 'ALACRITTY_WINDOW_ID') || set(env, 'ALACRITTY_SOCKET') || set(env, 'ALACRITTY_LOG') ? 'alacritty'
    : set(env, 'WEZTERM_PANE') || set(env, 'WEZTERM_EXECUTABLE') ? 'wezterm'
    : set(env, 'ITERM_SESSION_ID') || env['LC_TERMINAL'] === 'iTerm2' ? 'iterm2'
    : set(env, 'VSCODE_INJECTION') || set(env, 'VSCODE_GIT_IPC_HANDLE') || set(env, 'VSCODE_GIT_ASKPASS_MAIN')
      || set(env, 'VSCODE_IPC_HOOK_CLI') || set(env, 'CURSOR_TRACE_ID') ? editor(env)
    : undefined
  return { terminal: byTerm ?? byProgram ?? byMarker, tmux, remote }
}

/**
 * Tell VS Code from the editors built on it. Every one of them sets
 * `TERM_PROGRAM=vscode`; the path of the Git helper they export names the
 * application, and Cursor also exports `CURSOR_TRACE_ID`.
 */
function editor(env: Env): TerminalId {
  const hints = ['VSCODE_GIT_ASKPASS_MAIN', 'VSCODE_GIT_ASKPASS_NODE', '__CFBundleIdentifier']
    .map(name => env[name] ?? '').join('\n').toLowerCase()
  if (set(env, 'CURSOR_TRACE_ID') || hints.includes('cursor')) return 'cursor'
  if (hints.includes('windsurf')) return 'windsurf'
  if (hints.includes('codium')) return 'vscodium'
  if ((env['TERM_PROGRAM_VERSION'] ?? '').endsWith('-insider') || hints.includes('insiders')) return 'vscode-insiders'
  return 'vscode'
}

/** A Remote-SSH, WSL, or container window runs its server under one of these. */
function remoteEditor(env: Env): boolean {
  const paths = `${env['VSCODE_GIT_ASKPASS_MAIN'] ?? ''}\n${env['VSCODE_IPC_HOOK_CLI'] ?? ''}`
  return /[/\\]\.(?:vscode|vscode-insiders|cursor|windsurf|vscodium)-server[/\\]/u.test(paths)
}

/** A file format this module can edit. */
export type Format = 'keybindings' | 'windows-terminal' | 'ghostty' | 'kitty' | 'alacritty'

/** Where one terminal keeps its key bindings. */
export interface ConfigFiles {
  readonly format: Format
  /** The files the terminal reads, in the order it reads them. */
  readonly files: readonly string[]
  /**
   * Whether the terminal reads all of `files`, later ones overriding earlier
   * ones, rather than only the first that exists.
   */
  readonly layered: boolean
  /** The file to create when none exists; undefined when a missing file means the terminal is not installed here. */
  readonly create: string | undefined
  /** Superseded files that mean the user's config is in a format this module does not edit. */
  readonly legacy: readonly string[]
}

/** The parts of the process a path depends on. */
export interface Place {
  readonly env: Env
  readonly platform: NodeJS.Platform
  readonly home: string
}

/**
 * Where a terminal keeps its key bindings on this machine.
 * @param terminal - an identified terminal.
 * @param place - environment, platform, and home directory.
 * @returns its files, or undefined for a terminal whose configuration is not edited.
 */
export function configFiles(terminal: TerminalId, place: Place): ConfigFiles | undefined {
  const { env, platform, home } = place
  const join = platform === 'win32' ? win32.join : posix.join
  const xdg = env['XDG_CONFIG_HOME'] || join(home, '.config')
  switch (terminal) {
    case 'vscode': case 'vscode-insiders': case 'vscodium': case 'cursor': case 'windsurf': {
      // User settings live in Electron's appData directory: %APPDATA% on Windows,
      // ~/Library/Application Support on macOS, and $XDG_CONFIG_HOME or ~/.config on Linux
      // (https://www.electronjs.org/docs/latest/api/app#appgetpathname), under the product's
      // own name and `User`, beside settings.json
      // (https://code.visualstudio.com/docs/configure/settings#_settings-json-file).
      const product = {
        'vscode': 'Code', 'vscode-insiders': 'Code - Insiders', 'vscodium': 'VSCodium', 'cursor': 'Cursor', 'windsurf': 'Windsurf',
      }[terminal]
      const appData = platform === 'win32' ? env['APPDATA'] || join(home, 'AppData', 'Roaming')
        : platform === 'darwin' ? join(home, 'Library', 'Application Support') : xdg
      const file = join(appData, product, 'User', 'keybindings.json')
      return { format: 'keybindings', files: [file], layered: false, create: file, legacy: [] }
    }
    case 'windows-terminal': {
      // https://learn.microsoft.com/en-us/windows/terminal/install#settings-json-file: the Store
      // release, the Preview, then an unpackaged install (Scoop, Chocolatey). Windows Terminal
      // writes its settings on first run, so a missing file is an install that is not here.
      if (platform !== 'win32') return undefined
      const local = env['LOCALAPPDATA'] || join(home, 'AppData', 'Local')
      return {
        format: 'windows-terminal', layered: false, create: undefined, legacy: [], files: [
          join(local, 'Packages', 'Microsoft.WindowsTerminal_8wekyb3d8bbwe', 'LocalState', 'settings.json'),
          join(local, 'Packages', 'Microsoft.WindowsTerminalPreview_8wekyb3d8bbwe', 'LocalState', 'settings.json'),
          join(local, 'Microsoft', 'Windows Terminal', 'settings.json'),
        ],
      }
    }
    case 'ghostty': {
      // https://ghostty.org/docs/config#file-location: every one of these that exists is loaded,
      // in this order, later values overriding earlier ones; `config.ghostty` is the name from
      // 1.2.3, and plain `config` is still read.
      const mac = join(home, 'Library', 'Application Support', 'com.mitchellh.ghostty')
      const files = [join(xdg, 'ghostty', 'config.ghostty'), join(xdg, 'ghostty', 'config'),
        ...platform === 'darwin' ? [join(mac, 'config.ghostty'), join(mac, 'config')] : []]
      const version = /^(\d+)\.(\d+)\.(\d+)/u.exec(env['TERM_PROGRAM'] === 'ghostty' ? env['TERM_PROGRAM_VERSION'] ?? '' : '')
      const named = version !== null && Number(version[1]) * 1e6 + Number(version[2]) * 1e3 + Number(version[3]) >= 1_002_003
      return { format: 'ghostty', files, layered: true, create: join(xdg, 'ghostty', named ? 'config.ghostty' : 'config'), legacy: [] }
    }
    case 'kitty': {
      // https://sw.kovidgoyal.net/kitty/invocation/#cmdoption-kitty-config: KITTY_CONFIG_DIRECTORY
      // alone when set, otherwise the first kitty.conf of $XDG_CONFIG_HOME/kitty and
      // ~/.config/kitty; macOS also reads ~/Library/Preferences/kitty (tools/utils/paths.go).
      const fixed = env['KITTY_CONFIG_DIRECTORY']
      const files = fixed ? [join(fixed, 'kitty.conf')] : [...new Set([
        join(xdg, 'kitty', 'kitty.conf'), join(home, '.config', 'kitty', 'kitty.conf'),
        ...platform === 'darwin' ? [join(home, 'Library', 'Preferences', 'kitty', 'kitty.conf')] : [],
      ])]
      return { format: 'kitty', files, layered: false, create: files[0], legacy: [] }
    }
    case 'alacritty': {
      // https://alacritty.org/config-alacritty.html#location: the first of these that exists.
      const own = env['XDG_CONFIG_HOME']
      const bases = platform === 'win32' ? [join(env['APPDATA'] || join(home, 'AppData', 'Roaming'), 'alacritty', 'alacritty')]
        : [...own ? [join(own, 'alacritty', 'alacritty'), join(own, 'alacritty')] : [],
          join(home, '.config', 'alacritty', 'alacritty'), join(home, '.alacritty')]
      return { format: 'alacritty', files: bases.map(base => `${base}.toml`), layered: false, create: `${bases[0]!}.toml`,
        legacy: bases.map(base => `${base}.yml`) }
    }
    default: return undefined
  }
}

/** What a file already says about Shift+Enter. */
export type Found =
  | { readonly kind: 'present' }
  /** Shift+Enter is bound to something else; `existing` quotes the binding, on one line. */
  | { readonly kind: 'conflict'; readonly existing: string }

/** The outcome of editing one file. */
export type Edit =
  | Found
  /** The binding is added: `text` is the whole new file, and `snippet` the lines added, as the preview shows them. */
  | { readonly kind: 'write'; readonly text: string; readonly snippet: string }
  /**
   * The file cannot be edited safely: it does not parse (`unreadable`), or,
   * for Alacritty, keeps its bindings in an inline array a new table would
   * collide with (`inline`). The user adds `manualSnippet` by hand.
   */
  | { readonly kind: 'manual'; readonly reason: 'unreadable' | 'inline' }

/**
 * Add the Shift+Enter binding to one file, unless it is there already.
 * @param format - the file's format.
 * @param text - the file's contents, or undefined when it does not exist yet.
 * @returns what to do with the file.
 */
export function editConfig(format: Format, text: string | undefined): Edit {
  switch (format) {
    case 'keybindings': return editKeybindings(text)
    case 'windows-terminal': return editWindowsTerminal(text)
    case 'ghostty': return appendLines(text, findGhostty, [MANUAL_SNIPPETS.ghostty])
    case 'kitty': return appendLines(text, findKitty, [MANUAL_SNIPPETS.kitty])
    case 'alacritty': return editAlacritty(text)
  }
}

/**
 * What a file already binds Shift+Enter to.
 * @param format - the file's format.
 * @param text - the file's contents.
 * @returns the binding in force, or undefined when there is none or the file cannot be read.
 */
export function findBinding(format: Format, text: string): Found | undefined {
  switch (format) {
    case 'keybindings': { const found = findKeybindings(text); return found === 'unreadable' ? undefined : found }
    case 'windows-terminal': { const found = findWindowsTerminal(text); return found === 'unreadable' ? undefined : found }
    case 'ghostty': return findGhostty(text)
    case 'kitty': return findKitty(text)
    case 'alacritty': { const found = findAlacritty(text); return found.found }
  }
}

/* ------------------------------------------------------------------------ */
/* VS Code keybindings.json                                                  */
/* ------------------------------------------------------------------------ */

const SEND_SEQUENCE = 'workbench.action.terminal.sendSequence'

/** The rule VS Code, Cursor, and Windsurf take. `sendSequence` needs `\u001b`, not `\x1b`. */
function vscodeBinding() {
  return { key: 'shift+enter', command: SEND_SEQUENCE, args: { text: NEWLINE_SEQUENCE }, when: 'terminalFocus' }
}

/** A key chord's parts in one order: modifiers sorted, the key last. */
function normalizeKeys(keys: string, aliases: Readonly<Record<string, string>> = {}): string {
  const parts = keys.trim().toLowerCase().split('+').map(part => part.trim()).map(part => aliases[part] ?? part)
  const key = parts.pop() ?? ''
  return [...parts.sort(), key].join('+')
}

/** One line of source, however it was laid out. */
const oneLine = (source: string): string => source.replace(/\s+/gu, ' ').trim()

/**
 * The rule VS Code applies to Shift+Enter in the terminal. Rules are
 * evaluated bottom to top and the first that matches wins
 * (https://code.visualstudio.com/docs/configure/keybindings#_keyboard-rules),
 * so that is the last one here whose `when` can hold in a focused terminal.
 */
function findKeybindings(text: string): Found | undefined | 'unreadable' {
  let root: JsonNode | undefined
  try { root = parseJsonc(text) } catch { return 'unreadable' }
  if (root === undefined) return undefined
  if (root.type !== 'array') return 'unreadable'
  let found: Found | undefined
  for (const item of root.children ?? []) {
    const rule = valueOf(item)
    if (!isRecord(rule) || typeof rule['key'] !== 'string' || normalizeKeys(rule['key']) !== 'shift+enter') continue
    // `-command` removes a rule instead of adding one.
    if (typeof rule['command'] !== 'string' || rule['command'].startsWith('-')) continue
    const when = typeof rule['when'] === 'string' ? rule['when'] : ''
    if (when !== '' && !/(?<![!\w.])terminal/u.test(when)) continue
    const args = rule['args']
    found = rule['command'] === SEND_SEQUENCE && isRecord(args) && args['text'] === NEWLINE_SEQUENCE
      ? { kind: 'present' } : { kind: 'conflict', existing: oneLine(text.slice(item.offset, item.offset + item.length)) }
  }
  return found
}

function editKeybindings(text: string | undefined): Edit {
  const found = findKeybindings(text ?? '')
  if (found === 'unreadable') return { kind: 'manual', reason: 'unreadable' }
  if (found !== undefined) return found
  const layout = layoutOf(text ?? '')
  const render = (indent: string): string => `// ${MARK}${layout.eol}${indent}${blockJson(vscodeBinding(), indent, layout)}`
  const snippet = render('').replaceAll(layout.eol, '\n')
  const root = parseJsonc(text ?? '')
  if (root === undefined || root.type !== 'array') {
    const { eol, unit } = layout
    return { kind: 'write', text: `${lead(text, layout)}[${eol}${unit}${render(unit)}${eol}]${eol}`, snippet }
  }
  return { kind: 'write', text: appendItem(text!, root, render, layout), snippet }
}

/** The file so far, ending on a line of its own; nothing for a file that does not exist. */
function lead(text: string | undefined, layout: Layout): string {
  if (text === undefined || text.trim() === '') return ''
  return text.endsWith('\n') ? text : `${text}${layout.eol}`
}

/* ------------------------------------------------------------------------ */
/* Windows Terminal settings.json                                            */
/* ------------------------------------------------------------------------ */

function wtCommand() { return { action: 'sendInput', input: NEWLINE_SEQUENCE } }
/** The id this edit gives its action in the 1.21+ layout, where `keybindings` refers to actions by id. */
export const WT_ACTION_ID = 'User.Bake.ShiftEnterNewline'

/**
 * The Shift+Enter binding Windows Terminal applies: an action with inline
 * `keys`, or a `keybindings` entry naming an action's `id`
 * (https://learn.microsoft.com/en-us/windows/terminal/customize-settings/actions#keybindings).
 * An id of `null` or `unbound` passes the key through on purpose, which is a
 * choice this edit does not override.
 */
function findWindowsTerminal(text: string): Found | undefined | 'unreadable' {
  let root: JsonNode | undefined
  try { root = parseJsonc(text) } catch { return 'unreadable' }
  if (root === undefined) return undefined
  if (root.type !== 'object') return 'unreadable'
  const entries = (['actions', 'keybindings'] as const).flatMap(key => {
    const array = member(root, key)
    return array?.type === 'array' ? array.children ?? [] : []
  })
  const commands = new Map<string, unknown>()
  for (const entry of entries) {
    const value = valueOf(entry)
    if (isRecord(value) && typeof value['id'] === 'string' && value['command'] !== undefined) commands.set(value['id'], value['command'])
  }
  let found: Found | undefined
  for (const entry of entries) {
    const value = valueOf(entry)
    if (!isRecord(value)) continue
    const keys = typeof value['keys'] === 'string' ? [value['keys']] : Array.isArray(value['keys']) ? value['keys'] : []
    if (!keys.some(key => typeof key === 'string' && normalizeKeys(key) === 'shift+enter')) continue
    const command = value['command'] ?? (typeof value['id'] === 'string' ? commands.get(value['id']) : undefined)
    found = isRecord(command) && command['action'] === 'sendInput' && command['input'] === NEWLINE_SEQUENCE
      ? { kind: 'present' } : { kind: 'conflict', existing: oneLine(text.slice(entry.offset, entry.offset + entry.length)) }
  }
  return found
}

function editWindowsTerminal(text: string | undefined): Edit {
  if (text === undefined) return { kind: 'manual', reason: 'unreadable' }
  const found = findWindowsTerminal(text)
  if (found === 'unreadable') return { kind: 'manual', reason: 'unreadable' }
  if (found !== undefined) return found
  const root = parseJsonc(text)
  if (root?.type !== 'object') return { kind: 'manual', reason: 'unreadable' }
  const layout = layoutOf(text)
  const actions = member(root, 'actions')
  const keybindings = member(root, 'keybindings')
  if ((actions !== undefined && actions.type !== 'array') || (keybindings !== undefined && keybindings.type !== 'array')) {
    return { kind: 'manual', reason: 'unreadable' }
  }
  const item = (value: unknown) => (indent: string): string => `// ${MARK}${layout.eol}${indent}${inlineJson(value)}`
  const add = (source: string, key: 'actions' | 'keybindings', value: unknown): string => {
    const object = parseJsonc(source)!
    const array = member(object, key)
    if (array?.type === 'array') return appendItem(source, array, item(value), layout)
    const inner = (indent: string): string => indent + layout.unit
    return appendMember(source, object, key,
      indent => `[${layout.eol}${inner(indent)}${item(value)(inner(indent))}${layout.eol}${indent}]`, layout)
  }
  // 1.21 and later keep keys in `keybindings`, each naming an action's id; an
  // older file's `keybindings` is the former name of `actions`, holding commands.
  const split = keybindings?.type === 'array' && !(keybindings.children ?? []).some((entry) => {
    const value = valueOf(entry)
    return isRecord(value) && value['command'] !== undefined
  })
  if (split) {
    const action = { command: wtCommand(), id: WT_ACTION_ID }
    const binding = { id: WT_ACTION_ID, keys: 'shift+enter' }
    const ours = actions?.type === 'array' && (actions.children ?? []).some(entry => {
      const value = valueOf(entry)
      return isRecord(value) && value['id'] === WT_ACTION_ID
    })
    const withAction = ours ? text : add(text, 'actions', action)
    return {
      kind: 'write', text: add(withAction, 'keybindings', binding),
      snippet: [...ours ? [] : ['// "actions"', inlineJson(action)], '// "keybindings"', inlineJson(binding)].join('\n'),
    }
  }
  const entry = { command: wtCommand(), keys: 'shift+enter' }
  const key = actions === undefined && keybindings !== undefined ? 'keybindings' : 'actions'
  return { kind: 'write', text: add(text, key, entry), snippet: [`// "${key}"`, inlineJson(entry)].join('\n') }
}

/* ------------------------------------------------------------------------ */
/* Line-oriented files: Ghostty and kitty                                    */
/* ------------------------------------------------------------------------ */

/** Append the marked lines at the end of a file that does not bind Shift+Enter yet. */
function appendLines(text: string | undefined, find: (text: string) => Found | undefined, lines: readonly string[]): Edit {
  const found = find(text ?? '')
  if (found !== undefined) return found
  const layout = layoutOf(text ?? '')
  const added = [`# ${MARK}`, ...lines]
  return { kind: 'write', text: `${separated(text, layout)}${added.join(layout.eol)}${layout.eol}`, snippet: added.join('\n') }
}

/** The file so far, followed by one blank line when it has content. */
function separated(text: string | undefined, layout: Layout): string {
  const body = lead(text, layout)
  return body === '' || /(?:\r?\n){2}$/u.test(body) ? body : `${body}${layout.eol}`
}

const MODIFIERS: Readonly<Record<string, string>> = {
  control: 'ctrl', opt: 'alt', option: 'alt', cmd: 'super', command: 'super', return: 'enter',
}

/**
 * Ghostty's Shift+Enter binding: the last `keybind` whose trigger is
 * Shift+Enter, since a later one replaces an earlier one
 * (https://ghostty.org/docs/config/keybind#trigger-prefixes), and
 * `keybind = clear` drops everything before it.
 */
function findGhostty(text: string): Found | undefined {
  let found: Found | undefined
  for (const line of text.split(/\r?\n/u)) {
    const value = /^\s*keybind\s*=\s*(.*?)\s*$/u.exec(line)?.[1]?.replace(/^"(.*)"$/u, '$1')
    if (value === undefined) continue
    if (value === 'clear') { found = undefined; continue }
    // The trigger ends at the first `=` that is not itself the key, as in `ctrl+==…`.
    let split = value.indexOf('=')
    while (split > 0 && value[split - 1] === '+') split = value.indexOf('=', split + 1)
    if (split < 0) continue
    const trigger = value.slice(0, split).replace(/^(?:(?:all|global|unconsumed|performable):)+/u, '')
    if (trigger.includes('>') || normalizeKeys(trigger, MODIFIERS) !== 'shift+enter') continue
    const action = value.slice(split + 1).trim()
    found = /^text:/u.test(action) && decodeEscapes(action.slice('text:'.length)) === NEWLINE_SEQUENCE
      ? { kind: 'present' } : { kind: 'conflict', existing: line.trim() }
  }
  return found
}

/**
 * kitty's Shift+Enter mapping: the last unconditional `map` of it. A mapping
 * with `--when-focus-on` or a keyboard mode applies only there, so it is not
 * what Shift+Enter does in Bake.
 */
function findKitty(text: string): Found | undefined {
  let found: Found | undefined
  for (const line of text.split(/\r?\n/u)) {
    const match = /^\s*map\s+(\S+)\s*(.*?)\s*$/u.exec(line)
    if (match === null || match[1]!.startsWith('--') || match[1]!.includes('>')) continue
    if (normalizeKeys(match[1]!, MODIFIERS) !== 'shift+enter') continue
    const action = /^send_text\s+(\S+)\s+(.*)$/u.exec(match[2]!)
    const modes = action?.[1]?.split(',') ?? []
    found = action !== null && (modes.includes('all') || modes.includes('normal')) && decodeEscapes(action[2]!) === NEWLINE_SEQUENCE
      ? { kind: 'present' } : { kind: 'conflict', existing: line.trim() }
  }
  return found
}

/**
 * Decode the escapes these formats share: Zig string literals (Ghostty),
 * ANSI C escapes (kitty), and TOML basic strings (Alacritty).
 * @param text - the escaped text, without quotes.
 * @returns the characters it denotes.
 */
export function decodeEscapes(text: string): string {
  const simple: Readonly<Record<string, string>> = {
    'n': '\n', 'r': '\r', 't': '\t', 'e': '\x1b', 'b': '\b', 'f': '\f', '\\': '\\', '"': '"', '\'': '\'', '0': '\0',
  }
  return text.replace(/\\(x[0-9a-fA-F]{2}|u\{[0-9a-fA-F]+\}|u[0-9a-fA-F]{4}|U[0-9a-fA-F]{8}|.)/gu, (whole, escape: string) => {
    if (/^x/u.test(escape)) return String.fromCodePoint(Number.parseInt(escape.slice(1), 16))
    if (/^u\{/u.test(escape)) return String.fromCodePoint(Number.parseInt(escape.slice(2, -1), 16))
    if (/^[uU][0-9a-fA-F]/u.test(escape)) return String.fromCodePoint(Number.parseInt(escape.slice(1), 16))
    return simple[escape] ?? whole
  })
}

/* ------------------------------------------------------------------------ */
/* Alacritty alacritty.toml                                                  */
/* ------------------------------------------------------------------------ */

/** A TOML string value: a basic string with escapes, or a literal one without. */
function tomlString(value: string): string | undefined {
  const basic = /^"((?:[^"\\]|\\.)*)"/u.exec(value)
  if (basic !== null) return decodeEscapes(basic[1]!)
  return /^'([^']*)'/u.exec(value)?.[1]
}

/**
 * Alacritty's Shift+Enter bindings. Every binding of a key fires, in order
 * (https://alacritty.org/config-alacritty.html#keyboard), so any Shift+Enter
 * binding besides ESC CR would still send its own text: that is a conflict
 * wherever it is. `inline` says the file keeps its bindings in an inline
 * array, which a `[[keyboard.bindings]]` table would redefine.
 */
function findAlacritty(text: string): { readonly found: Found | undefined; readonly inline: boolean } {
  const bindings: { readonly fields: Record<string, string>; readonly source: string }[] = []
  let inline = false
  let table: string | undefined
  let current: { fields: Record<string, string>; source: string } | undefined
  const lines = text.split(/\r?\n/u)
  for (const [index, line] of lines.entries()) {
    const header = /^\s*\[\s*(\[)?\s*([^\]]+?)\s*\]/u.exec(line)
    if (header !== null) {
      table = header[2]!.replace(/\s+/gu, '')
      current = header[1] === '[' && table === 'keyboard.bindings' ? { fields: {}, source: line.trim() } : undefined
      if (current !== undefined) bindings.push(current)
      continue
    }
    const pair = /^\s*([\w.-]+)\s*=\s*(.*)$/u.exec(line)
    if (pair === null) continue
    const [, key, value] = pair as unknown as [string, string, string]
    if (current !== undefined) {
      current.fields[key] = value
      current.source += ` ${line.trim()}`
    } else if ((table === 'keyboard' && key === 'bindings')
      || (table === undefined && (key === 'keyboard' || key === 'keyboard.bindings'))) {
      inline = true
      // The inline array's `{ ... }` entries, up to its closing bracket.
      const rest = lines.slice(index).join('\n').slice(line.indexOf('=') + 1)
      for (const entry of inlineTables(rest)) bindings.push({ fields: entryFields(entry), source: oneLine(entry) })
    }
  }
  let present = false
  for (const { fields, source } of bindings) {
    const key = tomlString(fields['key'] ?? '')?.toLowerCase()
    const mods = (tomlString(fields['mods'] ?? '') ?? '').split('|').map(part => part.trim().toLowerCase())
      .map(part => MODIFIERS[part] ?? part)
    if ((key !== 'enter' && key !== 'return') || mods.length !== 1 || mods[0] !== 'shift') continue
    const ours = tomlString(fields['chars'] ?? '') === NEWLINE_SEQUENCE && fields['action'] === undefined && fields['command'] === undefined
    if (ours) present = true
    else return { found: { kind: 'conflict', existing: source }, inline }
  }
  return { found: present ? { kind: 'present' } : undefined, inline }
}

/** The top-level `{ ... }` entries of an inline TOML array, stopping at its closing bracket. */
function inlineTables(text: string): string[] {
  const tables: string[] = []
  let depth = 0
  let start = -1
  let quote: string | undefined
  for (let at = 0; at < text.length; at++) {
    const char = text[at]!
    if (quote !== undefined) {
      if (char === '\\' && quote === '"') at += 1
      else if (char === quote) quote = undefined
      continue
    }
    if (char === '"' || char === '\'') quote = char
    else if (char === '#') at = Math.max(at, text.indexOf('\n', at) < 0 ? text.length : text.indexOf('\n', at))
    else if (char === '[' || char === '{') { depth += 1; if (char === '{' && depth === 2) start = at }
    else if (char === ']' || char === '}') {
      depth -= 1
      if (char === '}' && depth === 1 && start >= 0) { tables.push(text.slice(start, at + 1)); start = -1 }
      if (depth === 0) break
    }
  }
  return tables
}

/** The `name = value` pairs of one inline table, values left encoded. */
function entryFields(entry: string): Record<string, string> {
  const fields: Record<string, string> = {}
  for (const match of entry.matchAll(/([\w-]+)\s*=\s*("(?:[^"\\]|\\.)*"|'[^']*'|\{[^}]*\}|[^,}\s]+)/gu)) fields[match[1]!] = match[2]!
  return fields
}

function editAlacritty(text: string | undefined): Edit {
  const { found, inline } = findAlacritty(text ?? '')
  if (found !== undefined) return found
  if (inline) return { kind: 'manual', reason: 'inline' }
  const layout = layoutOf(text ?? '')
  const added = [`# ${MARK}`, ...MANUAL_SNIPPETS.alacritty.split('\n')]
  return { kind: 'write', text: `${separated(text, layout)}${added.join(layout.eol)}${layout.eol}`, snippet: added.join('\n') }
}

/** The lines a user adds by hand, for each format, in the terminal's own syntax. */
export const MANUAL_SNIPPETS: Readonly<Record<Format | 'alacritty-inline' | 'alacritty-yaml' | 'wezterm', string>> = {
  // https://code.visualstudio.com/docs/terminal/advanced#_custom-sequence-keyboard-shortcuts
  'keybindings': inlineJson(vscodeBinding()),
  // https://learn.microsoft.com/en-us/windows/terminal/customize-settings/actions#send-input: an
  // action with inline keys, which every release reads and 1.21+ splits into `keybindings` itself.
  'windows-terminal': inlineJson({ command: wtCommand(), keys: 'shift+enter' }),
  // https://ghostty.org/docs/config/keybind/reference#text: `text:` takes a Zig string literal.
  'ghostty': 'keybind = shift+enter=text:\\x1b\\r',
  // https://sw.kovidgoyal.net/kitty/conf/#send-arbitrary-text-on-key-presses: the text decodes ANSI C escapes.
  'kitty': 'map shift+enter send_text all \\x1b\\r',
  // https://alacritty.org/config-alacritty.html#keyboard: TOML basic strings take \u escapes.
  'alacritty': '[[keyboard.bindings]]\nkey = "Enter"\nmods = "Shift"\nchars = "\\u001b\\r"',
  'alacritty-inline': '{ key = "Enter", mods = "Shift", chars = "\\u001b\\r" },',
  // alacritty.yml, before 0.13: `key_bindings` entries in YAML flow style.
  'alacritty-yaml': '- { key: Return, mods: Shift, chars: "\\x1b\\r" }',
  // https://wezterm.org/config/lua/keyassignment/SendString.html
  'wezterm': '{ key = \'Enter\', mods = \'SHIFT\', action = wezterm.action.SendString \'\\x1b\\r\' },',
}
