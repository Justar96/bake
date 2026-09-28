/**
 * File-reference discovery seam shared by host-backed user interfaces.
 *
 * @module @deepseek-ai/dsh-file-reference
 */

import { Service, type Context } from '@deepseek-ai/cordis'
import type { Agent } from '@deepseek-ai/dsh-agent'

import type { FileReferenceCandidate } from './types.ts'

export { activeAtToken, formatFileMention } from './grammar.ts'
export type { ActiveAtToken } from './grammar.ts'
export type { FileReferenceCandidate } from './types.ts'

/** Model guidance for path-only references selected by a user interface. */
export const FILE_REFERENCE_PROMPT = 'Tokens like @path are workspace paths the user referenced, relative to the workspace root; @"..." quotes a path with spaces. A path ending in / is a directory: list it when its contents matter. Any other path is a file: read it when you need its contents, and do not claim to know them before you read it.'

declare module '@deepseek-ai/cordis' {
  interface Context {
    fileReferences: FileReferenceService
  }
}

/** Host capability for cancellable file-reference discovery. */
export abstract class FileReferenceService extends Service {
  constructor(ctx: Context) {
    super(ctx, 'fileReferences')
  }

  /**
   * List file and directory candidates for one agent's working directory.
   * @param agent - target agent whose session cwd bounds discovery.
   * @param query - path text following `@` or `@"`.
   * @param signal - caller cancellation.
   * @returns deterministic path-only candidates.
   */
  abstract list(
    agent: Agent,
    query: string,
    signal: AbortSignal,
  ): Promise<FileReferenceCandidate[]>
}

export default FileReferenceService
