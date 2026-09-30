/**
 * Hand the terminal to the user's editor for one file, and take it back.
 *
 * The editor is `$VISUAL`, then `$EDITOR`, then `vi` (Notepad on Windows),
 * run through the shell the way Git runs it, so a command with arguments
 * such as `code --wait` works. Ink releases the terminal for the child's
 * lifetime: it erases its frame, leaves the alternate screen, and turns raw
 * mode and bracketed paste off, then redraws the whole frame once the child
 * exits. The file lives in a private temporary directory that is removed
 * whether or not the edit succeeds.
 *
 * @module @dsh-tui/app/external-editor
 */
import { spawn } from 'node:child_process'
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

/** Ink's `suspendTerminal` with a callback: the terminal is the callback's until it settles. */
export type SuspendTerminal = (callback: () => Promise<void>) => Promise<void>

/** Edit text in the user's editor. Resolves with the saved text; rejects when the editor fails. */
export type EditText = (text: string, name: string, signal: AbortSignal) => Promise<string>

/**
 * The command that opens the user's editor.
 * @param env - the process environment.
 * @param platform - the host platform.
 * @returns `$VISUAL`, then `$EDITOR`, then the platform's own editor.
 */
export function editorCommand(env: NodeJS.ProcessEnv, platform: NodeJS.Platform = process.platform): string {
  for (const name of ['VISUAL', 'EDITOR'] as const) {
    const value = env[name]?.trim()
    if (value !== undefined && value !== '') return value
  }
  return platform === 'win32' ? 'notepad' : 'vi'
}

/** The libuv handle under a TTY stream, as Node's `net.Socket` drives it. */
interface ReadHandle {
  reading?: boolean
  readStop?: () => number
  readStart?: () => number
}

/**
 * Stop this process reading the terminal while a child does.
 *
 * Ink's suspension turns raw mode off and detaches its listener, but the
 * stream's read stays armed on the file descriptor. In canonical mode each
 * finished line goes to whichever process reads first, so the editor would
 * lose lines to a buffer nobody reads. The handle's read is stopped and
 * restarted as `net.Socket#pause` does for a socket that owns its buffer;
 * the stream's own state still says a read is pending, which is true again
 * once it restarts.
 * @param stream - the terminal's stdin.
 * @returns restarts the read, when this stopped one.
 */
export function holdInput(stream: NodeJS.ReadStream): () => void {
  const handle = (stream as NodeJS.ReadStream & { _handle?: ReadHandle })._handle
  if (handle?.reading !== true || handle.readStop === undefined || handle.readStart === undefined) return () => {}
  handle.readStop()
  handle.reading = false
  return () => {
    handle.reading = true
    handle.readStart!()
  }
}

/**
 * Open one file in the user's editor while Ink has released the terminal.
 * @param suspend - Ink's terminal suspension.
 * @param text - the file's starting text.
 * @param name - the file's name, whose extension tells the editor its syntax.
 * @param signal - stops the editor and rejects.
 * @param env - the environment the editor is chosen from and runs in.
 * @returns the text the editor saved.
 */
export async function editExternally(suspend: SuspendTerminal, text: string, name: string, signal: AbortSignal,
  env: NodeJS.ProcessEnv = process.env): Promise<string> {
  signal.throwIfAborted()
  const dir = await mkdtemp(join(tmpdir(), 'bake-edit-'))
  try {
    const file = join(dir, name)
    await writeFile(file, text, { encoding: 'utf8', mode: 0o600 })
    const command = editorCommand(env)
    let outcome: { readonly code: number | null, readonly signal: NodeJS.Signals | null } | undefined
    await suspend(async () => { outcome = await runEditor(command, file, signal, env) })
    signal.throwIfAborted()
    if (outcome === undefined || outcome.code !== 0) {
      throw new Error(`${command} exited with ${outcome?.signal ?? outcome?.code ?? 'no status'}`)
    }
    return await readFile(file, 'utf8')
  } finally {
    await rm(dir, { recursive: true, force: true })
  }
}

/** Run the editor on the terminal until it exits; an abort stops it. */
function runEditor(command: string, file: string, signal: AbortSignal, env: NodeJS.ProcessEnv): Promise<{ code: number | null, signal: NodeJS.Signals | null }> {
  return new Promise((resolve, reject) => {
    // POSIX passes the path as `$1`, so no quoting of it is needed, and the
    // editor replaces the shell, so stopping the child stops the editor.
    const child = process.platform === 'win32'
      ? spawn(`${command} "${file}"`, { shell: true, stdio: 'inherit', env })
      : spawn('/bin/sh', ['-c', `exec ${command} "$1"`, 'editor', file], { stdio: 'inherit', env })
    const stop = (): void => { child.kill() }
    // The editor shares the terminal's process group. A Ctrl-C it does not
    // take for itself is its own to handle, as under Git, and never ends this process.
    const ignore = (): void => {}
    process.on('SIGINT', ignore)
    const settle = (): void => {
      signal.removeEventListener('abort', stop)
      process.off('SIGINT', ignore)
    }
    signal.addEventListener('abort', stop, { once: true })
    child.once('error', error => { settle(); reject(error) })
    child.once('close', (code, exit) => { settle(); resolve({ code, signal: exit }) })
  })
}
