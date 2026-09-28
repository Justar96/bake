/**
 * Deterministic raster fixtures and encoded-header probes for request-image
 * assertions, so specs can submit large images and read back the dimensions a
 * provider would decode without depending on an image library.
 */

import { deflateSync } from 'node:zlib'

const CRC_TABLE = Uint32Array.from({ length: 256 }, (_, index) => {
  let value = index
  for (let bit = 0; bit < 8; bit += 1) value = value & 1 ? 0xedb88320 ^ (value >>> 1) : value >>> 1
  return value >>> 0
})

function crc32(bytes: Uint8Array): number {
  let crc = 0xffffffff
  for (const byte of bytes) crc = (CRC_TABLE[(crc ^ byte) & 0xff] as number) ^ (crc >>> 8)
  return (crc ^ 0xffffffff) >>> 0
}

function pngChunk(type: string, data: Uint8Array): Buffer {
  const body = Buffer.concat([Buffer.from(type, 'latin1'), data])
  const length = Buffer.alloc(4)
  length.writeUInt32BE(data.byteLength)
  const crc = Buffer.alloc(4)
  crc.writeUInt32BE(crc32(body))
  return Buffer.concat([length, body, crc])
}

/**
 * Encode an opaque, metadata-free, single-colour 8-bit RGB PNG.
 * @param width - positive pixel width.
 * @param height - positive pixel height.
 * @param rgb - fill colour; distinct colours give distinct attachment ids.
 * @returns the complete PNG bytes.
 */
export function solidPng(width: number, height: number, rgb: readonly [number, number, number]): Uint8Array {
  const row = Buffer.alloc(1 + width * 3)
  for (let x = 0; x < width; x += 1) row.set(rgb, 1 + x * 3)
  const raw = Buffer.alloc(row.byteLength * height)
  for (let y = 0; y < height; y += 1) row.copy(raw, y * row.byteLength)
  const header = Buffer.alloc(13)
  header.writeUInt32BE(width, 0)
  header.writeUInt32BE(height, 4)
  header.set([8, 2, 0, 0, 0], 8)
  return new Uint8Array(Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    pngChunk('IHDR', header),
    pngChunk('IDAT', deflateSync(raw)),
    pngChunk('IEND', new Uint8Array()),
  ]))
}

/**
 * Decode every inline image a captured provider request body carries, in wire
 * order: Anthropic Messages `image` blocks with a base64 source, including
 * those nested in tool results, and OpenAI Responses `input_image` data URLs.
 * @param body - the parsed JSON request body.
 * @returns the decoded image bytes.
 */
export function wireImages(body: unknown): Uint8Array[] {
  const images: Uint8Array[] = []
  const visit = (value: unknown): void => {
    if (Array.isArray(value)) {
      for (const item of value) visit(item)
      return
    }
    if (typeof value !== 'object' || value === null) return
    const node = value as { type?: unknown; source?: { type?: unknown; data?: unknown }; image_url?: unknown }
    if (node.type === 'image' && node.source?.type === 'base64' && typeof node.source.data === 'string') {
      images.push(new Uint8Array(Buffer.from(node.source.data, 'base64')))
      return
    }
    if (node.type === 'input_image' && typeof node.image_url === 'string') {
      images.push(new Uint8Array(Buffer.from(node.image_url.slice(node.image_url.indexOf(',') + 1), 'base64')))
      return
    }
    for (const child of Object.values(value)) visit(child)
  }
  visit(body)
  return images
}

/** JPEG start-of-frame markers, which carry the frame dimensions. */
const JPEG_FRAME_MARKERS = new Set([0xc0, 0xc1, 0xc2, 0xc3, 0xc5, 0xc6, 0xc7, 0xc9, 0xca, 0xcb, 0xcd, 0xce, 0xcf])

/**
 * Read the pixel dimensions a decoder would produce from PNG or JPEG bytes.
 * @param data - complete encoded image bytes.
 * @returns the encoded frame's width and height.
 * @throws Error for any other format or a truncated header.
 */
export function encodedDimensions(data: Uint8Array): { width: number; height: number } {
  const bytes = Buffer.from(data.buffer, data.byteOffset, data.byteLength)
  if (bytes.readUInt32BE(0) === 0x89504e47) return { width: bytes.readUInt32BE(16), height: bytes.readUInt32BE(20) }
  if (bytes[0] === 0xff && bytes[1] === 0xd8) {
    let offset = 2
    while (offset + 9 <= bytes.byteLength) {
      if (bytes[offset] !== 0xff) throw new Error(`malformed JPEG marker at ${offset}`)
      const marker = bytes[offset + 1] as number
      if (JPEG_FRAME_MARKERS.has(marker)) {
        return { width: bytes.readUInt16BE(offset + 7), height: bytes.readUInt16BE(offset + 5) }
      }
      offset += 2 + bytes.readUInt16BE(offset + 2)
    }
    throw new Error('JPEG ends before its frame header')
  }
  throw new Error('unsupported image format')
}
