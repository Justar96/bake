/**
 * Syntax colour for response code blocks, source reads, search matches, structured tool output, and diffs, from Shiki.
 *
 * The presentation layer asks for colour synchronously, a run of lines at a
 * time, and Shiki loads its grammars asynchronously. This module is where
 * those two meet. The languages an agent edits most are loaded before the
 * first frame. A file in any other language loads its grammar the first time
 * one is drawn. Until a grammar is ready, its lines draw in their side's
 * tones, as they would with no highlighter at all. A committed row prints
 * once, so source drawn before its grammar loaded stays uncoloured in
 * scrollback. The next one is coloured.
 *
 * The theme is Solarized Dark because Solarized's accents are shared by its
 * light and dark variants, so they stay readable on either terminal
 * background. Its base tones are the one part that assumes a background, so
 * they are left uncoloured. The presentation layer draws them in the line's
 * own tone, and comments are dimmed.
 *
 * @module @dsh-tui/app/syntax
 */

import { createHighlighterCore, type HighlighterCore, type LanguageInput } from 'shiki/core'
import { createJavaScriptRegexEngine } from 'shiki/engine/javascript'
import { bundledLanguages, bundledLanguagesInfo } from 'shiki/langs'
import type { CodeToken, Highlight } from '@dsh-tui/ui'

/** Languages loaded before the first frame. These are what an agent in a repository edits most. */
const PRELOADED = ['typescript', 'tsx', 'javascript', 'jsx', 'json', 'markdown', 'python', 'rust', 'go', 'yaml', 'shellscript', 'css', 'html', 'toml'] as const

/** Extensions and file names whose language id is not the extension itself. */
const LANGUAGE_OF: Readonly<Record<string, string>> = {
  ts: 'typescript', mts: 'typescript', cts: 'typescript', js: 'javascript', mjs: 'javascript', cjs: 'javascript',
  jsonl: 'json', ndjson: 'json', jsonc: 'jsonc', json5: 'json5', md: 'markdown', py: 'python', rs: 'rust', yml: 'yaml',
  sh: 'shellscript', bash: 'shellscript', zsh: 'shellscript', dockerfile: 'docker', makefile: 'make',
}

/** Every bundled language id, by itself and by each of its aliases, so a grammar loads once under one name. */
const CANONICAL: ReadonlyMap<string, string> = new Map(bundledLanguagesInfo.flatMap(info =>
  [[info.id, info.id] as const, ...(info.aliases ?? []).map(alias => [alias, info.id] as const)]))

/** Solarized's base tones, which a theme draws as foreground on its own background. */
const FOREGROUND = new Set(['#839496', '#93a1a1', '#657b83', '#eee8d5', '#fdf6e3'])
/** Solarized's base01, the tone of comments. */
const SUPPORTING = '#586e75'

/** Highlighted runs kept for re-rendered rows, keyed by language and text. */
const CACHED = 128

/**
 * Code units of cached text, keys summed. Each run's tokens grow with its
 * text, so this bounds both. A streaming fence adds a near-copy of itself on
 * every delta; a count alone would let a few large ones hold megabytes.
 */
const CACHED_TEXT = 1024 * 1024

/** Shiki's `FontStyle.Italic`, a bit flag. */
const ITALIC = 1

/** A highlighter the presentation layer can call, and its lifecycle. */
export interface Syntax {
  /** Colour for consecutive lines of one file, or undefined until its grammar is ready. */
  readonly highlight: Highlight
  /** Settles when the preloaded languages are ready, or have failed. Never rejects. */
  readonly ready: Promise<void>
  /** Await grammars still loading, then release the highlighter. */
  close(): Promise<void>
}

/**
 * The language a file is highlighted as.
 * @param path - the file's path.
 * @returns a Shiki language id, or undefined when none is bundled.
 */
export function languageOf(path: string): string | undefined {
  const name = path.split(/[\\/]/).at(-1)!.toLowerCase()
  const extension = name.includes('.') ? name.slice(name.lastIndexOf('.') + 1) : name
  return CANONICAL.get(LANGUAGE_OF[extension] ?? extension)
}

/**
 * Start loading a highlighter.
 *
 * A load failure leaves text in its semantic tone and reports no diagnostic.
 *
 * @param limits - how many highlighted runs to keep, and how much text they may hold.
 * @returns the highlighter, usable at once.
 */
export function createSyntax(limits: { readonly runs: number, readonly text: number } = { runs: CACHED, text: CACHED_TEXT }): Syntax {
  let core: HighlighterCore | undefined
  let closed = false
  const loaded = new Set<string>()
  const loading = new Map<string, Promise<void>>()
  // Least recently used first: a hit moves its entry to the end.
  const cache = new Map<string, readonly (readonly CodeToken[])[]>()
  let cachedText = 0
  const creating = createHighlighterCore({
    themes: [import('shiki/themes/solarized-dark.mjs')],
    langs: PRELOADED.map(language => bundledLanguages[language] as LanguageInput),
    engine: createJavaScriptRegexEngine({ forgiving: true }),
  }).then(highlighter => {
    if (closed) {
      highlighter.dispose()
      return
    }
    core = highlighter
    for (const language of highlighter.getLoadedLanguages()) loaded.add(language)
  }, () => {
    // A load failure leaves the text in its semantic tone. See the module comment.
  })

  const load = (language: string): void => {
    if (core === undefined || closed || loading.has(language)) return
    const highlighter = core
    loading.set(language, highlighter.loadLanguage(bundledLanguages[language as keyof typeof bundledLanguages] as LanguageInput).then(() => {
      if (!closed) loaded.add(language)
    }, () => {
      // Left in `loading`, so a grammar that failed is not retried on every frame.
    }))
  }

  const highlight: Highlight = (lines, path) => {
    const language = languageOf(path)
    if (language === undefined || core === undefined || closed) return undefined
    if (!loaded.has(language)) {
      load(language)
      return undefined
    }
    const text = lines.join('\n')
    const key = `${language}\0${text}`
    const hit = cache.get(key)
    if (hit !== undefined) {
      cache.delete(key)
      cache.set(key, hit)
      return hit
    }
    let tokens: readonly (readonly CodeToken[])[]
    try {
      tokens = core.codeToTokensBase(text, { lang: language, theme: 'solarized-dark' })
        .map(line => line.map(token => codeToken(token.content.length, token.color, token.fontStyle)))
    } catch {
      // A grammar the regex engine cannot run leaves these lines plain.
      return undefined
    }
    // A run larger than the whole budget is drawn but not kept.
    if (key.length > limits.text) return tokens
    while (cache.size > 0 && (cache.size >= limits.runs || cachedText + key.length > limits.text)) {
      const oldest = cache.keys().next().value!
      cache.delete(oldest)
      cachedText -= oldest.length
    }
    cache.set(key, tokens)
    cachedText += key.length
    return tokens
  }

  return {
    highlight,
    ready: creating,
    async close() {
      closed = true
      await creating
      await Promise.all(loading.values())
      core?.dispose()
      core = undefined
      cache.clear()
      cachedText = 0
    },
  }
}

/**
 * One token as the presentation layer draws it.
 * @param length - UTF-16 length of the token.
 * @param color - the theme's colour for it.
 * @param fontStyle - Shiki's font style flags.
 * @returns the token, uncoloured where the theme used a base tone.
 */
function codeToken(length: number, color: string | undefined, fontStyle: number | undefined): CodeToken {
  const hex = color?.toLowerCase()
  return {
    length,
    ...hex === undefined || FOREGROUND.has(hex) || hex === SUPPORTING ? {} : { color: hex },
    ...hex === SUPPORTING ? { dim: true } : {},
    ...fontStyle !== undefined && fontStyle > 0 && (fontStyle & ITALIC) !== 0 ? { italic: true } : {},
  }
}
