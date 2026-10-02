/**
 * The `deepseek-official` profile the base bundle ships, read from its
 * `llm-pi-ai` row in packages/bundle/base/cordis.patch.yml, with the endpoint
 * as a parameter so wire tests can point it at a local server. The agent-loop
 * eval runs the same profile, so reading the shipped file keeps the two from
 * drifting.
 */

import { readFileSync } from 'node:fs'
import * as yaml from 'js-yaml'
import type { PiAiProviderProfile } from '@deepseek-ai/dsh-llm-pi-ai'

/**
 * The patch's Loader expressions are opaque here: only the `llm-pi-ai` row is
 * read, and it holds plain YAML.
 */
const PATCH_SCHEMA = yaml.DEFAULT_SCHEMA.extend([
  new yaml.Type('tag:yaml.org,2002:js', { kind: 'scalar', construct: (source: string) => source }),
])

/** The shipped profile, parsed once and never handed out uncopied. */
const SHIPPED: PiAiProviderProfile = (() => {
  const text = readFileSync(new URL('../../../bundle/base/cordis.patch.yml', import.meta.url), 'utf8')
  const patches = yaml.load(text, { schema: PATCH_SCHEMA }) as { insert?: { id?: string; config?: unknown }[] }[]
  const row = patches.flatMap(patch => patch.insert ?? []).find(entry => entry.id === 'llm-pi-ai')
  const profile = (row?.config as { providers?: Record<string, PiAiProviderProfile> } | undefined)?.providers?.['deepseek-official']
  if (profile?.baseURL === undefined) throw new Error('base cordis.patch.yml ships no llm-pi-ai deepseek-official profile with a baseURL')
  return profile
})()

/** The shipped DeepSeek endpoint for its Anthropic-format Messages API. */
export const DEEPSEEK_ANTHROPIC_BASE_URL: string = SHIPPED.baseURL!

/** The selector guidance the shipped route gives deepseek-v4-pro. */
export const DEEPSEEK_PRO_DESCRIPTION: string
  = SHIPPED.models?.find(model => model.id === 'deepseek-v4-pro')?.description ?? ''

/**
 * Build the shipped DeepSeek route profile.
 * @param baseURL - endpoint the route posts `/v1/messages` under.
 * @returns a fresh profile the caller may mutate.
 */
export function deepseekProfile(baseURL = DEEPSEEK_ANTHROPIC_BASE_URL): PiAiProviderProfile {
  return { ...structuredClone(SHIPPED), baseURL }
}
