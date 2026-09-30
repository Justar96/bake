/** Choosing a composer frame the terminal can actually draw. */
import { describe, expect, test } from 'bun:test'
import { resolveFrame, type FrameRequest } from '../src/frame.ts'

const ask = (env: FrameRequest['env'], overrides: Partial<FrameRequest> = {}) =>
  resolveFrame({ configured: 'auto', locale: 'en', env, platform: 'linux', systemLocale: () => 'en-US', ...overrides })

const utf8 = { LANG: 'en_US.UTF-8', TERM: 'xterm-256color' }

describe('resolveFrame', () => {
  test('draws the rounded frame on a UTF-8 terminal', () => {
    expect(ask(utf8)).toBe('round')
    expect(ask({ LC_ALL: 'en_GB.utf8', TERM: 'screen-256color' })).toBe('round')
  })

  test('falls back to ASCII when the terminal is not encoding UTF-8', () => {
    // Box-drawing characters written to a non-UTF-8 terminal come out as
    // mojibake on every row of the frame, which is worse than ASCII on all of
    // them. An unset locale is the C locale, which is not UTF-8 either.
    expect(ask({ LANG: 'en_US.ISO-8859-1', TERM: 'xterm' })).toBe('classic')
    expect(ask({ LANG: 'C', TERM: 'xterm' })).toBe('classic')
    expect(ask({ TERM: 'xterm' })).toBe('classic')
    expect(ask({ LANG: '', TERM: 'xterm' })).toBe('classic')
  })

  test('falls back to ASCII where the terminal renders nothing beyond text', () => {
    expect(ask({ ...utf8, TERM: 'dumb' })).toBe('classic')
    expect(ask({ LANG: utf8.LANG })).toBe('classic')
    expect(ask({ ...utf8, TERM: '' })).toBe('classic')
  })

  test('falls back to ASCII where Ambiguous characters may be drawn two cells wide', () => {
    // Not a capability, so it cannot be detected. A CJK character locale is
    // the signal that the terminal may be configured that way. The cost of
    // being wrong is a frame at twice the width Ink measured, which wraps and
    // leaves Ink's row arithmetic wrong from then on.
    expect(ask({ LANG: 'zh_CN.UTF-8', TERM: 'xterm-256color' })).toBe('classic')
    expect(ask({ LC_CTYPE: 'ja_JP.UTF-8', TERM: 'xterm-256color' })).toBe('classic')
    expect(ask({ LANG: 'ko_KR.UTF-8', TERM: 'xterm-256color' })).toBe('classic')
    // The interface locale stands in when the environment names none.
    expect(ask(utf8, { locale: 'zh' })).toBe('classic')
  })

  test('draws the rounded frame in Windows Terminal, which sets no locale or TERM', () => {
    const windows = { WT_SESSION: '0f3c9a52-3a0e-4d8e-9a55-6f1b1c2d3e4f' }
    expect(ask(windows)).toBe('round')
    expect(ask({ ...windows, LANG: 'C' })).toBe('round')
    // A CJK locale still names a terminal that may draw Ambiguous characters wide.
    expect(ask({ ...windows, LANG: 'zh_CN.UTF-8' })).toBe('classic')
    expect(ask(windows, { locale: 'zh' })).toBe('classic')
    expect(ask({ WT_SESSION: '' })).toBe('classic')
  })

  test('draws the rounded frame in any native Windows console', () => {
    // Node writes to the console as UTF-16 whatever the code page, and no
    // Windows console sets a locale or TERM, so their absence says nothing.
    const windows = { platform: 'win32' }
    expect(ask({}, windows)).toBe('round')
    expect(ask({ TERM_PROGRAM: 'vscode' }, windows)).toBe('round')
    expect(ask({ TERMINAL_EMULATOR: 'JetBrains-JediTerm' }, windows)).toBe('round')
    expect(ask({ TERM: 'dumb' }, windows)).toBe('classic')
    // The console host draws Ambiguous characters wide under a CJK code page,
    // and Windows names that locale in the system, not the environment.
    expect(ask({}, { ...windows, systemLocale: () => 'zh-CN' })).toBe('classic')
    expect(ask({}, { ...windows, systemLocale: () => 'ja-JP' })).toBe('classic')
    expect(ask({ LANG: 'ko_KR.UTF-8' }, windows)).toBe('classic')
    expect(ask({}, { ...windows, locale: 'zh' })).toBe('classic')
    // Windows Terminal draws them narrow, so the system locale does not apply there.
    expect(ask({ WT_SESSION: 'guid' }, { ...windows, systemLocale: () => 'zh-CN' })).toBe('round')
  })

  test('never reads the system locale outside native Windows', () => {
    const unread = { systemLocale: (): string => { throw new Error('read the system locale') } }
    expect(ask(utf8, unread)).toBe('round')
    expect(ask({}, { ...unread, platform: 'darwin' })).toBe('classic')
    expect(ask({ WT_SESSION: 'guid' }, { ...unread, platform: 'win32' })).toBe('round')
  })

  test('reads the variables in the order POSIX resolves them', () => {
    expect(ask({ LC_ALL: 'en_US.UTF-8', LC_CTYPE: 'zh_CN.UTF-8', LANG: 'C', TERM: 'xterm' })).toBe('round')
    expect(ask({ LC_CTYPE: 'zh_CN.UTF-8', LANG: 'en_US.UTF-8', TERM: 'xterm' })).toBe('classic')
  })

  test('lets the profile overrule the environment, which is what settings are for', () => {
    // No detection covers a terminal the environment describes wrongly.
    expect(ask({ LANG: 'C' }, { configured: 'round' })).toBe('round')
    expect(ask(utf8, { configured: 'classic' })).toBe('classic')
  })
})
