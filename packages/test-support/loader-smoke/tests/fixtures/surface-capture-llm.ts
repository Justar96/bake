import { writeFileSync } from 'node:fs'
import type { Context } from '@deepseek-ai/cordis'
import {
  LlmAdapter,
  ReasoningEffortId,
  type GenerateOptions,
  type LlmResolvedModelInfo,
  type StreamChunk,
} from 'bake-llm'

const HIGH = ReasoningEffortId('high')
const OFF = ReasoningEffortId('off')

/** Environment variable naming the JSON file the first loop request is written to. */
const CAPTURE_ENV = 'BAKE_SURFACE_CAPTURE'

/**
 * Keyless adapter that records the first agent-loop request it receives, as the
 * provider-neutral options every real adapter maps to its wire, then answers
 * every request with one line of text. Auxiliary calls (titles, summaries)
 * carry a `purpose` and are answered without being recorded.
 */
class SurfaceCaptureAdapter extends LlmAdapter {
  private captured = false

  override async resolveModel(provider: string, model: string): Promise<LlmResolvedModelInfo> {
    return {
      provider,
      id: model,
      name: model,
      reasoning: {
        efforts: [
          { id: OFF, name: 'Off' },
          { id: HIGH, name: 'High' },
        ],
        defaultEffort: HIGH,
      },
    }
  }

  async * stream(options: GenerateOptions): AsyncIterable<StreamChunk> {
    const path = process.env[CAPTURE_ENV]
    if (!this.captured && options.purpose === undefined && path !== undefined && path !== '') {
      this.captured = true
      const { signal: _signal, sessionId: _sessionId, ...request } = options
      writeFileSync(path, `${JSON.stringify(request, null, 2)}\n`)
    }
    const reply = 'Surface captured.'
    yield { type: 'block-start', index: 0, blockType: 'text' }
    yield { type: 'text-delta', index: 0, text: reply }
    yield { type: 'block-end', index: 0, block: { type: 'text', text: reply } }
    yield { type: 'usage', usage: { inputTokens: 1, outputTokens: 1 } }
    yield { type: 'finish', reason: { kind: 'stop' } }
  }
}

export const name = 'surface-capture-llm'
export const inject = ['llm']

/** Register the keyless `surface-capture` adapter. */
export function apply(ctx: Context): void {
  ctx.llm.registerAdapter(['surface-capture'], new SurfaceCaptureAdapter())
}
