/** Read an image from the system clipboard through the platform's own clipboard tools. */
import { execFile } from 'node:child_process'
import { mkdtemp, readFile, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import type { ImageMediaType } from '@deepseek-ai/dsh-attachment'

/** An image the clipboard held. */
export interface ClipboardImage {
  readonly data: Uint8Array
  readonly mediaType: ImageMediaType
}

/**
 * Run one program and collect its standard output.
 * @param file - program name, looked up on PATH.
 * @param args - its arguments.
 * @param signal - kills the program when aborted.
 * @returns standard output; rejects when the program is missing, fails, or is aborted.
 */
export type RunProgram = (file: string, args: readonly string[], signal: AbortSignal) => Promise<Uint8Array>

/** Largest clipboard image read; larger images are refused by attachment admission anyway. */
const MAX_BYTES = 64 * 1024 * 1024

const run: RunProgram = (file, args, signal) => new Promise((resolve, reject) => {
  execFile(file, [...args], { encoding: 'buffer', maxBuffer: MAX_BYTES, signal, windowsHide: true },
    (error, stdout) => { if (error !== null) reject(error); else resolve(new Uint8Array(stdout)) })
})

const TYPES: readonly ImageMediaType[] = ['image/png', 'image/jpeg', 'image/webp', 'image/gif']

/**
 * The first image type a clipboard lists, in Bake's preference order.
 * @param listing - newline-separated MIME types or X11 targets.
 * @returns the type to request, or undefined when the clipboard holds no image.
 */
export function clipboardImageType(listing: string): ImageMediaType | undefined {
  const offered = new Set(listing.split(/\r?\n/u).map(line => line.trim().toLowerCase()))
  return TYPES.find(type => offered.has(type))
}

/**
 * Read the clipboard's image, if it holds one.
 *
 * Wayland uses `wl-paste` and X11 `xclip`. macOS asks AppleScript for PNG data
 * and writes it to a private temporary file, and Windows asks PowerShell for
 * the clipboard bitmap as PNG. A missing tool reads as no image.
 *
 * @param signal - cancels a running tool.
 * @param options - the platform, environment, and program runner; this process's by default.
 * @returns the image bytes and type, or undefined when there is none or it cannot be read.
 */
export async function readClipboardImage(signal: AbortSignal, options: {
  readonly platform?: NodeJS.Platform
  readonly env?: NodeJS.ProcessEnv
  readonly run?: RunProgram
} = {}): Promise<ClipboardImage | undefined> {
  const platform = options.platform ?? process.platform
  const env = options.env ?? process.env
  const exec = options.run ?? run
  const attempt = async (read: () => Promise<ClipboardImage | undefined>): Promise<ClipboardImage | undefined> => {
    try { return await read() } catch { signal.throwIfAborted(); return undefined }
  }
  const nonEmpty = (data: Uint8Array, mediaType: ImageMediaType): ClipboardImage | undefined =>
    data.byteLength === 0 ? undefined : { data, mediaType }
  if (platform === 'darwin') {
    return attempt(async () => {
      const directory = await mkdtemp(join(tmpdir(), 'bake-clipboard-'))
      try {
        const file = join(directory, 'clipboard.png')
        await exec('osascript', ['-e', 'set png to (the clipboard as «class PNGf»)',
          '-e', `set f to open for access POSIX file ${JSON.stringify(file)} with write permission`,
          '-e', 'write png to f', '-e', 'close access f'], signal)
        return nonEmpty(new Uint8Array(await readFile(file)), 'image/png')
      } finally { await rm(directory, { recursive: true, force: true }) }
    })
  }
  if (platform === 'win32') {
    return attempt(async () => nonEmpty(await exec('powershell', ['-NoProfile', '-NonInteractive', '-Command',
      'Add-Type -AssemblyName System.Windows.Forms,System.Drawing; $i = [Windows.Forms.Clipboard]::GetImage(); '
      + 'if ($i) { $m = New-Object IO.MemoryStream; $i.Save($m, [Drawing.Imaging.ImageFormat]::Png); '
      + '$o = [Console]::OpenStandardOutput(); $o.Write($m.ToArray(), 0, $m.Length); $o.Flush() }'], signal), 'image/png'))
  }
  const decoder = new TextDecoder()
  if (env['WAYLAND_DISPLAY'] !== undefined && env['WAYLAND_DISPLAY'] !== '') {
    const image = await attempt(async () => {
      const type = clipboardImageType(decoder.decode(await exec('wl-paste', ['--list-types'], signal)))
      return type === undefined ? undefined : nonEmpty(await exec('wl-paste', ['--no-newline', '--type', type], signal), type)
    })
    if (image !== undefined) return image
  }
  return attempt(async () => {
    const type = clipboardImageType(decoder.decode(await exec('xclip', ['-selection', 'clipboard', '-t', 'TARGETS', '-o'], signal)))
    return type === undefined ? undefined
      : nonEmpty(await exec('xclip', ['-selection', 'clipboard', '-t', type, '-o'], signal), type)
  })
}
