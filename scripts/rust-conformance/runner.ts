/**
 * The Bun arm of the synthetic conformance comparison. It reads one input
 * document from stdin, applies each write whose permission allows it inside
 * the current directory, and prints one observation line. It implements no
 * permission policy: decisions arrive as fixture data.
 *
 * Exit 0 on success, 2 for an input that breaks the contract, 1 for an I/O
 * failure.
 */

import { mkdir, writeFile } from 'node:fs/promises'
import { dirname, join } from 'node:path'
import {
  ConformanceInputError, MAX_DOCUMENT_BYTES, OBSERVATION_SCHEMA, parseStrictJson, validateInput,
  type Observation, type SyntheticInput,
} from './fixture.ts'

/**
 * Apply the allowed writes under `cwd` and return the observation that relays
 * the input's prompts, events, and permissions unchanged.
 * @param input - a validated input.
 * @param cwd - the private workspace the driver created.
 */
export async function execute(input: SyntheticInput, cwd: string): Promise<Observation> {
  const decisions = new Map(input.permissions.map(entry => [entry.id, entry.decision]))
  for (const write of input.writes) {
    if (decisions.get(write.permission) !== 'allow') continue
    const target = join(cwd, ...write.path.split('/'))
    await mkdir(dirname(target), { recursive: true })
    await writeFile(target, write.text, 'utf8')
  }
  return {
    schema: OBSERVATION_SCHEMA, version: 1,
    prompts: input.prompts, events: input.events, permissions: input.permissions,
  }
}

/**
 * Read stdin to EOF, refusing more than the document bound.
 * @param stream - the input stream.
 */
export async function readBounded(stream: AsyncIterable<Uint8Array>): Promise<Uint8Array> {
  const chunks: Uint8Array[] = []
  let size = 0
  for await (const chunk of stream) {
    size += chunk.byteLength
    if (size > MAX_DOCUMENT_BYTES) throw new ConformanceInputError(`stdin: exceeds ${MAX_DOCUMENT_BYTES} bytes`)
    chunks.push(chunk)
  }
  return Buffer.concat(chunks)
}

/**
 * Parse and validate stdin, returning the input before any write runs.
 * @param stream - the input stream.
 */
export async function readInput(stream: AsyncIterable<Uint8Array>): Promise<SyntheticInput> {
  return validateInput(parseStrictJson(await readBounded(stream), 'stdin'))
}

async function main(): Promise<number> {
  let input: SyntheticInput
  try {
    input = await readInput(process.stdin)
  } catch (error) {
    if (!(error instanceof ConformanceInputError)) {
      process.stderr.write(`I/O failure: ${(error as Error).message}\n`)
      return 1
    }
    process.stderr.write(`invalid input: ${error.message}\n`)
    return 2
  }
  try {
    const observation = await execute(input, process.cwd())
    await new Promise<void>((resolve, reject) =>
      process.stdout.write(`${JSON.stringify(observation)}\n`, error => error ? reject(error) : resolve()))
  } catch (error) {
    process.stderr.write(`I/O failure: ${(error as Error).message}\n`)
    return 1
  }
  return 0
}

if (import.meta.main) process.exitCode = await main()
