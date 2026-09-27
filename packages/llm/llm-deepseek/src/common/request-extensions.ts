/** Prepare plugin-contributed request fields and commit their delivery after HTTP acceptance. */

import { LlmError } from '@deepseek-ai/dsh-llm'
import type { DeepSeekLlmApiExtensionRequest, PreparedDeepSeekLlmApiExtensions } from '@deepseek-ai/dsh-deepseek-llm-api-extensions'
import type { DeepSeekAdapterOptions } from './types.ts'

/**
 * Merge contributions without replacing protocol-owned fields. Preparation and
 * acceptance failures retain the same error category across DeepSeek protocols.
 * If the merged request fails to serialize, send the base request alone and
 * skip acceptance so contributors can resend their state on a later request.
 * @param body - serialized protocol request before extension fields.
 * @param options - request identity, purpose, and cancellation.
 * @param prepare - contributor registry captured for this adapter.
 * @param onOmitted - receives omitted field names and the serialization failure.
 * @returns HTTP payload and a commit to invoke only after a successful HTTP response.
 */
export async function prepareRequestExtensions(
  body: DeepSeekLlmApiExtensionRequest['body'],
  options: Omit<DeepSeekLlmApiExtensionRequest, 'body'>,
  prepare: DeepSeekAdapterOptions['prepareExtensions'],
  onOmitted: (fields: readonly string[], error: unknown) => void,
): Promise<{ payload: string; accept(): Promise<void> }> {
  let extensions: PreparedDeepSeekLlmApiExtensions
  try {
    extensions = await prepare({ body, ...options })
  } catch (error) {
    throw new LlmError('DeepSeek request extension preparation failed', 'REQUEST_EXTENSION', { cause: error })
  }
  const fields = Object.keys(extensions.fields)
  for (const field of fields) {
    if (Object.hasOwn(body, field)) {
      throw new LlmError(`DeepSeek request extension field ${JSON.stringify(field)} collides with the base request`, 'REQUEST_EXTENSION')
    }
  }
  let payload: string
  try {
    payload = JSON.stringify({ ...body, ...extensions.fields })
  } catch (error) {
    // Failure of the base request still fails the call without blaming extensions.
    const base = JSON.stringify(body)
    onOmitted(fields, error)
    return { payload: base, accept: () => Promise.resolve() }
  }
  return {
    payload,
    async accept() {
      try {
        await extensions.accept()
      } catch (error) {
        throw new LlmError('DeepSeek request extension acceptance failed', 'REQUEST_EXTENSION', { cause: error })
      }
    },
  }
}
