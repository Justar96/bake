/** Cancellable path discovery delegated to the session's Harness provider. */
import type { Agent } from '@deepseek-ai/dsh-agent'
import type {} from '@deepseek-ai/dsh-file-reference'
import type { FileCatalog } from '@dsh-tui/ui'
import type { TuiCopy } from '@dsh-tui/ui/copy.ts'

/** One terminal's active file query; discovery never changes the session log. */
export class FileReferences {
  private state: FileCatalog = { query: undefined, entries: [], loading: false, error: undefined }
  private abort: AbortController | undefined
  private closed = false
  private readonly pending = new Set<Promise<void>>()

  /**
   * Bind discovery to the exact session displayed by this terminal.
   * @param agent - agent whose scoped provider and cwd own the search.
   * @param copy - localized unavailable-provider diagnostic.
   * @param changed - renderer notification after an observation changes.
   */
  constructor(private readonly agent: Agent, private readonly copy: TuiCopy, private readonly changed: () => void) {}

  /** Paths for the current query, never file contents. */
  get view(): FileCatalog { return this.state }

  /**
   * Replace the active query and abort superseded discovery.
   * @param query - text after the active `@` token, or undefined to close discovery.
   */
  search(query: string | undefined): void {
    if (this.closed || query === this.state.query) return
    this.abort?.abort()
    const abort = this.abort = new AbortController()
    this.state = { query, entries: [], loading: query !== undefined, error: undefined }
    this.changed()
    if (query === undefined) return
    const done = Promise.resolve().then(async () => {
      abort.signal.throwIfAborted()
      const provider = this.agent.ctx.get('fileReferences')
      if (provider === undefined) throw new Error(this.copy.filesUnavailable)
      const entries = await provider.list(this.agent, query, abort.signal)
      abort.signal.throwIfAborted()
      this.state = { query, entries, loading: false, error: undefined }
      this.changed()
    }).catch((error: unknown) => {
      if (abort.signal.aborted) return
      this.state = { query, entries: [], loading: false, error: error instanceof Error ? error.message : String(error) }
      this.changed()
    }).finally(() => { this.pending.delete(done) })
    this.pending.add(done)
  }

  /** Abort discovery and prevent publication after terminal release. */
  close(): void { this.closed = true; this.abort?.abort() }

  /** @returns after every owned provider call has settled. */
  async drain(): Promise<void> { await Promise.all(this.pending) }
}
