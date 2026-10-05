/**
 * The workspace's git branch and uncommitted changes, as one status-line field.
 * @module bake-tui-ui/git
 */

import { PALETTE } from './palette.ts'
import { RANK, type StatusField, type StatusPart } from './status-line.ts'

/** The working tree's branch and its changes, read by the application from `git status`. */
export interface GitState {
  /** The checked-out branch, or the abbreviated commit when HEAD is detached. */
  readonly branch: string
  /** HEAD names a commit, not a branch. */
  readonly detached: boolean
  /** Commits on the branch that its upstream does not have. */
  readonly ahead: number
  /** Commits on the upstream that the branch does not have. */
  readonly behind: number
  /** Paths with changes in the index. */
  readonly staged: number
  /** Tracked paths with changes not yet staged. */
  readonly modified: number
  /** Paths git does not track and does not ignore. */
  readonly untracked: number
  /** Paths with unresolved merge conflicts. */
  readonly conflicted: number
}

/** Glyphs for the field; `ascii` where the terminal draws the classic frame. */
const GLYPHS = {
  unicode: { branch: '\u2387 ', ahead: '\u2191', behind: '\u2193' },
  ascii: { branch: '', ahead: '^', behind: 'v' },
} as const

/**
 * The branch, then its changes: `⎇ main +2 ~3 ?1 ↑1`.
 *
 * Staged paths are green, since they are ready to commit, unstaged changes
 * yellow, since they wait on the user, and conflicts red. Untracked paths and
 * the distance from the upstream stay dim: neither is work in progress on a
 * tracked file. A clean tree shows the branch alone. When the whole field
 * does not fit, it narrows to the branch, which is what the counts qualify,
 * at {@link RANK}'s `gitCounts`, and the branch goes at its `branch`.
 *
 * @param state - the working tree, as the application last read it.
 * @param glyphs - `ascii` where the terminal draws the classic frame.
 * @returns the status field.
 */
export function gitField(state: GitState, glyphs: 'unicode' | 'ascii'): StatusField {
  const glyph = GLYPHS[glyphs]
  const branch: StatusPart[] = [
    ...glyph.branch === '' ? [] : [{ text: glyph.branch, dim: true }],
    { text: state.detached ? `(${state.branch})` : state.branch },
  ]
  const counts: StatusPart[] = [
    ...state.conflicted === 0 ? [] : [{ text: `!${state.conflicted}`, color: PALETTE.failed }],
    ...state.staged === 0 ? [] : [{ text: `+${state.staged}`, color: PALETTE.done }],
    ...state.modified === 0 ? [] : [{ text: `~${state.modified}`, color: PALETTE.waiting }],
    ...state.untracked === 0 ? [] : [{ text: `?${state.untracked}`, dim: true }],
    ...state.ahead === 0 ? [] : [{ text: `${glyph.ahead}${state.ahead}`, dim: true }],
    ...state.behind === 0 ? [] : [{ text: `${glyph.behind}${state.behind}`, dim: true }],
  ]
  if (counts.length === 0) return { forms: [branch], yields: [RANK.branch] }
  return { forms: [[...branch, ...counts.map(part => ({ ...part, text: ` ${part.text}` }))], branch], yields: [RANK.gitCounts, RANK.branch] }
}
