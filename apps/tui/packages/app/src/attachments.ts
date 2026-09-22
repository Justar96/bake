/** Explicit file reads and prompt admission through the Agent's Harness services. */
import type { Agent, ModelSelectionRef } from '@deepseek-ai/dsh-agent'
import type { FileBlock, ImageBlock } from '@deepseek-ai/dsh-llm'
import type {} from '@deepseek-ai/dsh-fs'
import type { ImageMediaType, SaveImageAttachment } from '@deepseek-ai/dsh-attachment'
import type { AttachmentSummary } from '@dsh-tui/ui/rows.ts'
import type { TuiCopy } from '@dsh-tui/ui/copy.ts'

/** Validated limits on the TUI's in-memory attachment draft. */
export interface AttachmentOptions {
  readonly attachmentMaxBytes: number
  readonly attachmentLimit: number
}

interface Staged {
  readonly data: Uint8Array
  readonly name: string
  readonly mediaType?: ImageMediaType
}

/** Source bytes staged for one session. The controller serializes mutations; Harness owns storage and normalization. */
export class AttachmentDraft {
  private items: readonly Staged[] = []

  /**
   * @param agent - owner of filesystem, attachment storage, and model services.
   * @param options - resolved draft count and byte limits.
   * @param copy - locale-owned errors.
   */
  constructor(private readonly agent: Agent, private readonly options: AttachmentOptions, private readonly copy: TuiCopy) {}

  /** Source metadata, without bytes or storage paths. */
  get view(): readonly AttachmentSummary[] {
    return this.items.map(item => ({ name: item.name, bytes: item.data.byteLength,
      ...item.mediaType === undefined ? {} : { mediaType: item.mediaType } }))
  }

  /** Whether the next ordinary prompt has staged attachments. */
  get pending(): boolean { return this.items.length > 0 }

  /**
   * Read one complete path through the scoped filesystem and validate raster sources.
   * @param path - the entire command remainder, including literal spaces.
   * @param signal - command lifetime; late provider results never enter the draft.
   */
  async add(path: string, signal: AbortSignal): Promise<void> {
    signal.throwIfAborted()
    if (path === '') throw new Error(this.copy.attachUsage)
    if (this.items.length >= this.options.attachmentLimit) throw new Error(this.copy.attachmentCountLimit)
    const fs = this.agent.ctx.get('fs')
    const store = this.agent.ctx.get('attachments')
    if (fs === undefined || store === undefined) throw new Error(this.copy.attachmentsUnavailable)
    const target = await fs.resolve(path, { ...this.agent.session.header.cwd === undefined ? {} : { cwd: this.agent.session.header.cwd }, signal })
    const name = path.split(/[\\/]/).at(-1)!
    const extension = name.split('.').at(-1)?.toLowerCase()
    const types: Readonly<Record<string, ImageMediaType>> = { png: 'image/png', jpg: 'image/jpeg', jpeg: 'image/jpeg', webp: 'image/webp', gif: 'image/gif' }
    const mediaType = extension === undefined ? undefined : types[extension]
    const remaining = this.options.attachmentMaxBytes - this.items.reduce((sum, item) => sum + item.data.byteLength, 0)
    const data = await fs.readBytes(target, signal, mediaType === undefined ? remaining : Math.min(remaining, store.imageLimits.maxImageBytes))
    signal.throwIfAborted()
    if (mediaType !== undefined) await store.validateImage({ data, name, mediaType })
    signal.throwIfAborted()
    this.items = [...this.items, { data, name, ...mediaType === undefined ? {} : { mediaType } }]
  }

  /**
   * Remove one staged source without deleting a stored object.
   * @param position - one-based position displayed by the UI.
   */
  remove(position: string): void {
    const index = Number(position) - 1
    if (!/^\d+$/.test(position) || !Number.isSafeInteger(index) || this.items[index] === undefined) throw new Error(this.copy.removeAttachmentUsage)
    this.items = this.items.filter((_item, offset) => offset !== index)
  }

  /** Discard the draft; committed attachments remain owned by Harness storage. */
  clear(): void { this.items = [] }

  /**
   * Validate the current route and admit all images as one batch before returning ordered blocks.
   * @param selection - current Harness model selection, when installed.
   * @param signal - prompt admission lifetime; storage calls without cancellation are drained.
   * @returns durable blocks; the draft remains until the Agent accepts the message.
   */
  async admit(selection: ModelSelectionRef | undefined, signal: AbortSignal): Promise<readonly (ImageBlock | FileBlock)[]> {
    signal.throwIfAborted()
    const store = this.agent.ctx.get('attachments')
    const llm = this.agent.ctx.get('llm')
    if (store === undefined || llm === undefined) throw new Error(this.copy.attachmentsUnavailable)
    const images: SaveImageAttachment[] = this.items.flatMap(item => item.mediaType === undefined ? [] : [{ ...item, mediaType: item.mediaType }])
    const route = selection?.current
    if (images.length > 0) {
      if (route === undefined) throw new Error(this.copy.noModelSelection)
      const model = await llm.resolveModelInfo(route.provider, route.model, signal)
      signal.throwIfAborted()
      if (model.inputModalities !== undefined && !model.inputModalities.includes('image')) throw new Error(this.copy.modelNoImages)
    }
    const refs = images.length === 0 ? [] : await store.saveImages(images)
    signal.throwIfAborted()
    const result: (ImageBlock | FileBlock)[] = []
    let imageIndex = 0
    for (const item of this.items) {
      if (item.mediaType === undefined) result.push({ type: 'file', attachment: await store.saveFile(item) })
      else result.push({ type: 'image', attachment: refs[imageIndex++]! })
      signal.throwIfAborted()
    }
    if (selection?.current !== route) throw new Error(this.copy.attachmentModelChanged)
    return result
  }
}
