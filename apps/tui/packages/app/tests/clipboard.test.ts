/** Clipboard image reads and text writes choose each platform's tool; reads treat failures as no image, writes fall back to OSC 52. */
import { expect, it } from 'bun:test'
import { clipboardImageType, osc52, readClipboardImage, writeClipboardText, type FeedProgram, type RunProgram } from '../src/clipboard.ts'

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

function feeders(failing: readonly string[] = []): { readonly feed: FeedProgram, readonly calls: string[], readonly inputs: Uint8Array[] } {
  const calls: string[] = []
  const inputs: Uint8Array[] = []
  return {
    calls, inputs,
    feed: async (file, args, input) => {
      calls.push([file, ...args].join(' '))
      inputs.push(input)
      if (failing.includes(file)) throw new Error(`${file} failed`)
    },
  }
}

it('writes text with each platform\'s own tool, clip as UTF-16', async () => {
  const mac = feeders()
  expect(await writeClipboardText('héllo', undefined, { platform: 'darwin', env: {}, feed: mac.feed })).toBe(true)
  expect(mac.calls).toEqual(['pbcopy'])
  expect(new TextDecoder().decode(mac.inputs[0])).toBe('héllo')
  const windows = feeders()
  expect(await writeClipboardText('hé', undefined, { platform: 'win32', env: {}, feed: windows.feed })).toBe(true)
  expect(windows.calls).toEqual(['clip'])
  expect([...windows.inputs[0]!]).toEqual([0xff, 0xfe, 0x68, 0x00, 0xe9, 0x00])
  const wayland = feeders()
  await writeClipboardText('x', undefined, { platform: 'linux', env: { WAYLAND_DISPLAY: 'wayland-0' }, feed: wayland.feed })
  expect(wayland.calls).toEqual(['wl-copy'])
})

it('tries the next X11 tool, then OSC 52, when one fails', async () => {
  const written: string[] = []
  const x11 = feeders(['xclip'])
  expect(await writeClipboardText('x', sequence => { written.push(sequence) }, { platform: 'linux', env: {}, feed: x11.feed })).toBe(true)
  expect(x11.calls).toEqual(['xclip -selection clipboard', 'xsel --clipboard --input'])
  expect(written).toEqual([])
  const none = feeders(['xclip', 'xsel'])
  expect(await writeClipboardText('copied', sequence => { written.push(sequence) }, { platform: 'linux', env: {}, feed: none.feed })).toBe(true)
  expect(written).toEqual(['\x1b]52;c;Y29waWVk\x07'])
  // Without a terminal to write to, nothing got there.
  expect(await writeClipboardText('copied', undefined, { platform: 'linux', env: {}, feed: none.feed })).toBe(false)
})

it('sends OSC 52 first over SSH, and under Windows Terminal in WSL', async () => {
  for (const env of [{ SSH_TTY: '/dev/pts/1' }, { WSL_DISTRO_NAME: 'Ubuntu', WT_SESSION: 'id' }]) {
    const written: string[] = []
    const tools = feeders()
    expect(await writeClipboardText('é', sequence => { written.push(sequence) }, { platform: 'linux', env, feed: tools.feed })).toBe(true)
    expect(tools.calls).toEqual([])
    expect(written).toEqual([osc52('é')])
  }
  const wsl = feeders()
  await writeClipboardText('x', undefined, { platform: 'linux', env: { WSL_DISTRO_NAME: 'Ubuntu' }, feed: wsl.feed })
  expect(wsl.calls).toEqual(['clip.exe'])
})
