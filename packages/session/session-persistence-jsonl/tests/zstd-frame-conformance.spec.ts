/**
 * Runs the shared cases in `conformance/session/zstd-frame-cases.json`
 * through the backend's own `compressZstdFrame`: each input's frame must be
 * the listed hex, or have the listed length and SHA-256. The Rust Zstd
 * writer's `compress_zstd_frame` in `rust/crates/bake-session` checks the
 * same table, so both runtimes write the same frame bytes. The bytes depend
 * on the libzstd version, which the table names: on any other version both
 * arms fail on the version instead of a frame difference. The spec reads
 * only the table.
 */

import { createHash } from 'node:crypto'
import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'
import { compressZstdFrame, decompressZstdFrame } from '../src/zstd.ts'

const REPO = new URL('../../../../', import.meta.url)
const SCHEMA = 'bake/session-conformance/zstd-frame-cases'
const ORACLE = 'compress the input\'s bytes with the JSONL backend\'s compressZstdFrame, Node\'s asynchronous zstdCompress with ZSTD_c_checksumFlag set; list the frame\'s hex when it is at most 512 bytes, otherwise its length and SHA-256'
/** Both harnesses pin the table size, so a dropped case fails. */
const CASE_COUNT = 13

/** The libzstd version the table's frames were recorded with. */
let libzstd = ''

type Input = { text: string } | { repeat: string; times: number } | { xorshift: number; bytes: number } | { rows: number }
interface FrameCase {
  id: string
  input: Input
  inputBytes: number
  frameHex?: string
  frameBytes?: number
  frameSha256?: string
}

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function sortedKeys(value: object): string {
  return Object.keys(value).sort().join()
}

/** The bytes an input stands for; see the table's history for the generators. */
function inputBytes(input: unknown, id: string): Buffer {
  if (!isObject(input)) throw new Error(`${id}: invalid input`)
  switch (sortedKeys(input)) {
    case 'text':
      if (typeof input.text === 'string') return Buffer.from(input.text)
      break
    case 'repeat,times':
      if (typeof input.repeat === 'string' && Number.isSafeInteger(input.times)) return Buffer.from(input.repeat.repeat(input.times as number))
      break
    case 'rows':
      if (Number.isSafeInteger(input.rows)) {
        const rows: string[] = []
        for (let k = 0; k < (input.rows as number); k++) rows.push(`{"type":"turn/start","seq":${k},"time":${k + 1},"data":{"turn":${k}}}\n`)
        return Buffer.from(rows.join(''))
      }
      break
    case 'bytes,xorshift':
      if (Number.isSafeInteger(input.bytes) && Number.isSafeInteger(input.xorshift)) {
        const out = Buffer.alloc(input.bytes as number)
        let x = (input.xorshift as number) >>> 0
        for (let index = 0; index < out.length; index++) {
          x = (x ^ (x << 13)) >>> 0
          x = (x ^ (x >>> 17)) >>> 0
          x = (x ^ (x << 5)) >>> 0
          out[index] = x & 0xff
        }
        return out
      }
      break
  }
  throw new Error(`${id}: invalid input ${JSON.stringify(input)}`)
}

function loadTable(): FrameCase[] {
  const table: unknown = JSON.parse(readFileSync(new URL('conformance/session/zstd-frame-cases.json', REPO), 'utf8'))
  if (!isObject(table) || sortedKeys(table) !== 'cases,history,libzstd,oracle,schema,version' || table.schema !== SCHEMA
    || table.version !== 1 || table.oracle !== ORACLE || typeof table.libzstd !== 'string' || !Array.isArray(table.cases)
    || !Array.isArray(table.history) || !table.history.every(line => typeof line === 'string')) {
    throw new Error('zstd-frame-cases.json does not match its version-1 schema')
  }
  libzstd = table.libzstd
  return table.cases.map((entry: unknown): FrameCase => {
    if (!isObject(entry) || typeof entry.id !== 'string' || !Number.isSafeInteger(entry.inputBytes)) {
      throw new Error(`invalid case ${JSON.stringify(entry)}`)
    }
    const keys = sortedKeys(entry)
    if (keys === 'frameHex,id,input,inputBytes,note' && typeof entry.frameHex === 'string') {
      return { id: entry.id, input: entry.input as Input, inputBytes: entry.inputBytes as number, frameHex: entry.frameHex }
    }
    if (keys === 'frameBytes,frameSha256,id,input,inputBytes,note' && Number.isSafeInteger(entry.frameBytes)
      && typeof entry.frameSha256 === 'string') {
      return {
        id: entry.id, input: entry.input as Input, inputBytes: entry.inputBytes as number,
        frameBytes: entry.frameBytes as number, frameSha256: entry.frameSha256,
      }
    }
    throw new Error(`${entry.id}: invalid case keys ${keys}`)
  })
}

const cases = loadTable()

describe('shared Zstd frame cases', () => {
  it('pin the table', () => {
    expect(cases).toHaveLength(CASE_COUNT)
    expect(new Set(cases.map(entry => entry.id)).size).toBe(CASE_COUNT)
  })

  it('run on the libzstd the table was recorded with', () => {
    expect(process.versions.zstd, `Node bundles libzstd ${process.versions.zstd}; the frames were recorded with ${libzstd}`).toBe(libzstd)
  })

  for (const entry of cases) {
    it(entry.id, async () => {
      if (process.versions.zstd !== libzstd) throw new Error(`libzstd ${process.versions.zstd} is not the table's ${libzstd}`)
      const input = inputBytes(entry.input, entry.id)
      expect(input.length).toBe(entry.inputBytes)
      const frame = await compressZstdFrame(input)
      if (entry.frameHex !== undefined) {
        expect(frame.toString('hex')).toBe(entry.frameHex)
      } else {
        expect(frame.length).toBe(entry.frameBytes)
        expect(createHash('sha256').update(frame).digest('hex')).toBe(entry.frameSha256)
      }
      expect((await decompressZstdFrame(frame)).equals(input)).toBe(true)
    })
  }
})
