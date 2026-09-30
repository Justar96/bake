/**
 * The user's editor: chosen from `$VISUAL` and `$EDITOR`, run on a private
 * temporary file while the terminal is suspended, and read back once it exits.
 */
import { afterEach, describe, expect, it } from 'vitest'
import { chmod, mkdtemp, readdir, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { editExternally, editorCommand, type SuspendTerminal } from '../src/external-editor.ts'

const owned: string[] = []
afterEach(async () => { for (const dir of owned.splice(0)) await rm(dir, { recursive: true, force: true }) })

/** An editor script in a test-owned directory. */
async function script(body: string): Promise<{ readonly command: string, readonly dir: string }> {
  const dir = await mkdtemp(join(tmpdir(), 'bake-editor-test-'))
  owned.push(dir)
  const command = join(dir, 'editor.sh')
  await writeFile(command, `#!/bin/sh\n${body}\n`)
  await chmod(command, 0o755)
  return { command, dir }
}

/** The test's environment with only the editor it names, never the developer's own. */
function envWith(editor: Record<string, string>): NodeJS.ProcessEnv {
  const { VISUAL: _visual, EDITOR: _editor, ...rest } = process.env
  return { ...rest, ...editor }
}

/** A suspension that records when it held the terminal. */
function suspension() {
  const held: boolean[] = []
  const suspend: SuspendTerminal = async callback => {
    held.push(true)
    try { await callback() } finally { held.push(false) }
  }
  return { suspend, held }
}

describe('editorCommand', () => {
  it('prefers VISUAL, then EDITOR, then the platform editor', () => {
    expect(editorCommand({ VISUAL: 'code --wait', EDITOR: 'nano' })).toBe('code --wait')
    expect(editorCommand({ VISUAL: ' ', EDITOR: 'nano' })).toBe('nano')
    expect(editorCommand({}, 'linux')).toBe('vi')
    expect(editorCommand({}, 'win32')).toBe('notepad')
  })
})

describe.skipIf(process.platform === 'win32')('editExternally', () => {
  it('returns what the editor saved, with its arguments kept, and removes the file', async () => {
    const { command } = await script('shift_arg="$1"; last=""; for arg in "$@"; do last="$arg"; done; printf \'%s|%s\' "$shift_arg" "$(cat "$last")" > "$last"; echo "$last" > "$(dirname "$0")/seen"')
    const { suspend, held } = suspension()
    const text = await editExternally(suspend, '["a"]', 'tool.hosts.json', new AbortController().signal, envWith({ EDITOR: `${command} --wait` }))
    expect(text).toMatch(/^--wait\|\["a"\]$/)
    expect(held).toEqual([true, false])
    const file = (await readFile(join(owned[0]!, 'seen'), 'utf8')).trim()
    expect(file.endsWith('tool.hosts.json')).toBe(true)
    await expect(readdir(join(file, '..'))).rejects.toMatchObject({ code: 'ENOENT' })
  })

  it('rejects when the editor fails, and still resumes the terminal', async () => {
    const { command } = await script('exit 3')
    const { suspend, held } = suspension()
    await expect(editExternally(suspend, '{}', 'x.json', new AbortController().signal, envWith({ VISUAL: command })))
      .rejects.toThrow(`${command} exited with 3`)
    expect(held).toEqual([true, false])
  })

  it('stops the editor when the command is cancelled', async () => {
    const { command } = await script('exec sleep 30')
    const { suspend } = suspension()
    const abort = new AbortController()
    const edit = editExternally(suspend, '{}', 'x.json', abort.signal, envWith({ EDITOR: command }))
    setTimeout(() => { abort.abort(new Error('closed')) }, 100)
    await expect(edit).rejects.toThrow('closed')
  })
})
