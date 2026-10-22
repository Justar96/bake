/**
 * `/terminal-setup`: make Shift+Enter insert a new line in the prompt by
 * binding it, in the user's terminal, to ESC CR, which Bake already reads
 * as a new line.
 *
 * The command names the terminal it found, shows the file and the lines it
 * would add, and writes only after the user accepts, backing the file up
 * first. Running it again finds the binding and changes nothing. Terminals
 * whose configuration is a program or a property list (WezTerm, iTerm2,
 * Apple Terminal) get the steps instead of an edit, as does any terminal it
 * cannot identify and any whose files are on another machine. The parsing
 * and the edits are in `terminal-bindings.ts`; this module reads, asks, and
 * writes.
 *
 * @module bake-tui-app/terminal-setup
 */
import { constants } from 'node:fs'
import { copyFile, readFile, realpath, stat } from 'node:fs/promises'
import { homedir } from 'node:os'
import { writeFileAtomic } from 'bake-atomic-write'
import type { CommandResult } from 'bake-commands'
import type { TuiCopy } from 'bake-tui-ui/copy.ts'
import type { ChoicePrompt } from 'bake-tui-ui/picker.tsx'
import { compactPath } from 'bake-tui-ui/present.ts'
import {
  configFiles, detectTerminal, editConfig, EDITORS, findBinding, MANUAL_SNIPPETS, TERMINAL_NAMES,
  type Format, type Found, type Place, type TerminalId,
} from './terminal-bindings.ts'

/** What the command reads from the process; tests supply their own home and environment. */
export interface TerminalHost extends Place {
  /** Milliseconds since the epoch, for the backup's name. */
  readonly now: () => number
}

/** @returns this process's environment, platform, and home directory. */
export function processHost(): TerminalHost {
  return { env: process.env, platform: process.platform, home: homedir(), now: Date.now }
}

/** The one question the command asks. */
export type Ask = (prompt: ChoicePrompt, signal: AbortSignal) => Promise<string | undefined>

/** A file's name where the steps cannot name its path on the user's own machine. */
const FILE_NAMES: Readonly<Record<Format, string>> = {
  'keybindings': 'keybindings.json', 'windows-terminal': 'settings.json', 'ghostty': 'config.ghostty',
  'kitty': 'kitty.conf', 'alacritty': 'alacritty.toml',
}

/** Formats by terminal, for the steps shown where no file is edited. */
const FORMATS: Partial<Readonly<Record<TerminalId, Format>>> = {
  'windows-terminal': 'windows-terminal', 'ghostty': 'ghostty', 'kitty': 'kitty', 'alacritty': 'alacritty',
}

/**
 * Run `/terminal-setup` once.
 * @param host - environment, platform, home directory, and clock.
 * @param ask - the terminal's picker, for the confirmation.
 * @param copy - localized text.
 * @param signal - the command's lifetime.
 * @returns the report the transcript keeps.
 */
export async function terminalSetup(host: TerminalHost, ask: Ask, copy: TuiCopy, signal: AbortSignal): Promise<CommandResult> {
  const detection = detectTerminal(host.env)
  const preface = detection.tmux ? [copy.terminalSetupTmux] : []
  const done = (kind: 'success' | 'error', lines: readonly string[]): CommandResult => ({ kind, text: [...preface, ...lines].join('\n') })
  const terminal = detection.terminal
  if (terminal === undefined) return done('success', [copy.terminalSetupUnknown, copy.terminalSetupGeneric])
  const name = TERMINAL_NAMES[terminal]
  const config = detection.remote ? undefined : configFiles(terminal, host)
  if (config === undefined) {
    return done('success', [...detection.remote ? [`${name} · ${copy.terminalSetupRemote}`] : [], ...steps(terminal, copy)])
  }
  const shown = (path: string): string => compactPath(path, host.home)
  const manual = (path: string, format: Format | 'alacritty-inline' = config.format): string[] =>
    [`${copy.terminalSetupManual} ${shown(path)}:`, ...indented(MANUAL_SNIPPETS[format])]
  const report = (outcome: Found, path: string): CommandResult => outcome.kind === 'present'
    ? done('success', [`${name} · ${copy.terminalSetupPresent}`, shown(path)])
    : done('error', [`${name} · ${copy.terminalSetupConflict}`, `${shown(path)}:`, `  ${outcome.existing}`,
      copy.terminalSetupReplace, ...indented(MANUAL_SNIPPETS[config.format])])

  const found = await readExisting(config.files)
  signal.throwIfAborted()
  if (found.length === 0) {
    const legacy = (await readExisting(config.legacy))[0]
    if (legacy !== undefined) {
      return done('success', [`${name} · ${copy.terminalSetupAlacrittyYaml}`, `${shown(legacy.file)}:`,
        ...indented(MANUAL_SNIPPETS['alacritty-yaml'])])
    }
    if (config.create === undefined) return done('success', [`${name} · ${copy.terminalSetupNoFile}`, ...steps(terminal, copy)])
  }
  // A layered terminal reads every file; the binding in force is the last one found.
  if (config.layered) {
    let inForce: { found: Found; file: string } | undefined
    for (const { file, text } of found) {
      const binding = findBinding(config.format, text)
      if (binding !== undefined) inForce = { found: binding, file }
    }
    if (inForce !== undefined) return report(inForce.found, inForce.file)
  }
  const target = config.layered ? found.at(-1) : found[0]
  const file = target?.file ?? config.create!
  const edit = editConfig(config.format, target?.text)

  if (edit.kind === 'present' || edit.kind === 'conflict') return report(edit, file)
  if (edit.kind === 'manual') {
    return edit.reason === 'inline'
      ? done('success', [`${name} · ${copy.terminalSetupAlacrittyInline}`, ...manual(file, 'alacritty-inline')])
      : done('error', [`${name} · ${copy.terminalSetupUnreadable}`, ...manual(file)])
  }

  const existed = target !== undefined
  const choice = await ask({
    title: `${copy.terminalSetupTitle} · ${name}`, initial: 'write',
    warning: [...preface, `${existed ? copy.terminalSetupAdds : copy.terminalSetupCreates} ${shown(file)}`, ...indented(edit.snippet)]
      .join('\n'),
    choices: [
      { value: 'write', label: copy.terminalSetupWrite, description: existed ? copy.terminalSetupBackupFirst : copy.terminalSetupNewFile },
      { value: 'cancel', label: copy.terminalSetupCancel, description: copy.terminalSetupUnchanged },
    ],
  }, signal)
  signal.throwIfAborted()
  if (choice !== 'write') return { kind: 'success', text: copy.terminalSetupCancelled }

  // The file may have changed while the question was open. Plan again from
  // what is on disk now; the lines added are the same ones the user accepted.
  const current = (await readExisting([file]))[0]?.text
  const next = current === target?.text ? edit : editConfig(config.format, current)
  if (next.kind === 'present' || next.kind === 'conflict') return report(next, file)
  if (next.kind === 'manual') return done('error', [`${name} · ${copy.terminalSetupUnreadable}`, ...manual(file)])
  const created = current === undefined
  try {
    // Write through a symlink, as dotfile managers link these files, rather than replace it.
    const real = created ? file : await realpath(file)
    const mode = created ? 0o644 : (await stat(real)).mode & 0o777
    const backup = created ? undefined : await backUp(real, host.now())
    await writeFileAtomic(real, next.text, { mode })
    const posix = host.platform !== 'win32'
    const undo = backup === undefined
      ? posix ? `rm ${quote(real, posix)}` : `Remove-Item ${quote(real, posix)}`
      : posix ? `cp ${quote(backup, posix)} ${quote(real, posix)}` : `Copy-Item -Force ${quote(backup, posix)} ${quote(real, posix)}`
    return done('success', [
      `${name} · ${copy.terminalSetupDone}`,
      `${created ? copy.terminalSetupCreated : copy.terminalSetupWrote} ${shown(file)}`,
      ...backup === undefined ? [] : [`${copy.terminalSetupBackup} ${shown(backup)}`],
      `${copy.terminalSetupUndo} ${undo}`,
      reload(terminal, name, created, copy),
      copy.terminalSetupTest,
    ])
  } catch (error) {
    const reason = error instanceof Error ? error.message : String(error)
    return done('error', [`${name} · ${copy.terminalSetupFailed}: ${reason}`, ...manual(file)])
  }
}

/** The files among `paths` that exist, in order, with their text. */
async function readExisting(paths: readonly string[]): Promise<{ readonly file: string; readonly text: string }[]> {
  const read: { file: string; text: string }[] = []
  for (const file of paths) {
    try {
      read.push({ file, text: await readFile(file, 'utf8') })
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== 'ENOENT' && (error as NodeJS.ErrnoException).code !== 'ENOTDIR') throw error
    }
  }
  return read
}

/**
 * Copy a file beside itself as `<file>.bak-<ms>`, the name Bake's profile
 * recovery gives a replaced patch, adding `-1`, `-2` if that name is taken.
 * The copy never replaces an existing file.
 * @returns the backup's path.
 */
async function backUp(file: string, now: number): Promise<string> {
  const base = `${file}.bak-${now}`
  for (let ordinal = 0; ; ordinal++) {
    const backup = ordinal === 0 ? base : `${base}-${ordinal}`
    try {
      await copyFile(file, backup, constants.COPYFILE_EXCL)
      return backup
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== 'EEXIST') throw error
    }
  }
}

/** A path as one shell word: POSIX single quotes, or PowerShell's. */
function quote(path: string, posix: boolean): string {
  return posix ? `'${path.replaceAll('\'', '\'\\\'\'')}'` : `'${path.replaceAll('\'', '\'\'')}'`
}

/** Snippet lines, set in by two spaces under the line that introduces them. */
function indented(snippet: string): string[] {
  return snippet.split('\n').map(line => `  ${line}`)
}

/**
 * What the user does before Shift+Enter works.
 *
 * VS Code watches keybindings.json and applies it while running
 * (https://code.visualstudio.com/docs/configure/keybindings#_keyboard-rules).
 * Alacritty reloads a changed config by default (`live_config_reload`,
 * https://alacritty.org/config-alacritty.html#general). kitty reloads one that
 * existed when it started (`auto_reload_config`,
 * https://sw.kovidgoyal.net/kitty/conf/#opt-kitty.auto_reload_config), and
 * otherwise on ctrl+shift+f5. Ghostty reloads on ctrl+shift+, or cmd+shift+,
 * (https://ghostty.org/docs/config#reloading-the-configuration). Windows
 * Terminal's documentation does not say when it reads settings.json again,
 * so the hint there is to restart it if the key still submits.
 */
function reload(terminal: TerminalId, name: string, created: boolean, copy: TuiCopy): string {
  if (terminal === 'ghostty') return copy.terminalSetupReloadGhostty
  if (terminal === 'kitty' && created) return copy.terminalSetupReloadKitty
  if (terminal === 'windows-terminal') return `${copy.terminalSetupRestart} ${name}`
  return `${name} ${copy.terminalSetupLive}`
}

/**
 * The steps for a terminal whose files are not edited here: a Lua or
 * property-list configuration, a terminal on another machine, or one with no
 * settings file where it would be.
 */
function steps(terminal: TerminalId, copy: TuiCopy): string[] {
  const format = FORMATS[terminal]
  if (EDITORS.has(terminal)) return [copy.terminalSetupEditorSteps, ...indented(MANUAL_SNIPPETS.keybindings)]
  if (terminal === 'wezterm') return [copy.terminalSetupWezterm, ...indented(MANUAL_SNIPPETS.wezterm), copy.terminalSetupAlways]
  if (terminal === 'iterm2') return [copy.terminalSetupIterm, copy.terminalSetupItermOption, copy.terminalSetupAlways]
  if (terminal === 'apple-terminal') return [copy.terminalSetupApple, copy.terminalSetupAlways]
  if (format === 'windows-terminal') return [copy.terminalSetupWindowsTerminal, ...indented(MANUAL_SNIPPETS['windows-terminal'])]
  return format === undefined ? [copy.terminalSetupGeneric]
    : [`${copy.terminalSetupManual} ${FILE_NAMES[format]}:`, ...indented(MANUAL_SNIPPETS[format])]
}
