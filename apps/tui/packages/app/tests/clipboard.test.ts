/** Clipboard image reads choose each platform's tool and treat failures as no image. */
import { expect, it } from 'bun:test'
import { clipboardImageType, readClipboardImage, type RunProgram } from '../src/clipboard.ts'

const bytes = new Uint8Array([137, 80, 78, 71])
const encode = (text: string): Uint8Array => new TextEncoder().encode(text)

function programs(outputs: Record<string, Uint8Array | Error>): { readonly run: RunProgram, readonly calls: string[] } {
  const calls: string[] = []
  return {
    calls,
    run: async (file, args) => {
      const call = [file, ...args].join(' ')
      calls.push(call)
      const output = outputs[call]
      if (output === undefined || output instanceof Error) throw output ?? new Error(`unexpected ${call}`)
      return output
    },
  }
}

it('prefers PNG among the listed types and ignores non-image targets', () => {
  expect(clipboardImageType('TARGETS\ntext/plain\nimage/jpeg\nimage/png\n')).toBe('image/png')
  expect(clipboardImageType('text/plain\nUTF8_STRING')).toBeUndefined()
})

it('reads a Wayland image, falling back to X11 when wl-paste has none', async () => {
  const signal = new AbortController().signal
  const wayland = programs({ 'wl-paste --list-types': encode('image/jpeg\ntext/plain'), 'wl-paste --no-newline --type image/jpeg': bytes })
  expect(await readClipboardImage(signal, { platform: 'linux', env: { WAYLAND_DISPLAY: 'wayland-0' }, run: wayland.run }))
    .toEqual({ data: bytes, mediaType: 'image/jpeg' })
  const x11 = programs({
    'wl-paste --list-types': new Error('missing'),
    'xclip -selection clipboard -t TARGETS -o': encode('TARGETS\nimage/png'),
    'xclip -selection clipboard -t image/png -o': bytes,
  })
  expect(await readClipboardImage(signal, { platform: 'linux', env: { WAYLAND_DISPLAY: 'wayland-0' }, run: x11.run }))
    .toEqual({ data: bytes, mediaType: 'image/png' })
})

it('reads no image from a text clipboard or when no tool is installed', async () => {
  const signal = new AbortController().signal
  const text = programs({ 'xclip -selection clipboard -t TARGETS -o': encode('UTF8_STRING') })
  expect(await readClipboardImage(signal, { platform: 'linux', env: {}, run: text.run })).toBeUndefined()
  expect(text.calls).toEqual(['xclip -selection clipboard -t TARGETS -o'])
  expect(await readClipboardImage(signal, { platform: 'linux', env: {}, run: programs({}).run })).toBeUndefined()
})

it('reads a PowerShell bitmap as PNG on Windows', async () => {
  const run: RunProgram = async file => { if (file !== 'powershell') throw new Error(file); return bytes }
  expect(await readClipboardImage(new AbortController().signal, { platform: 'win32', env: {}, run }))
    .toEqual({ data: bytes, mediaType: 'image/png' })
})
