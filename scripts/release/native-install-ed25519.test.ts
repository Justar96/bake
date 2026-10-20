/**
 * The Ed25519 verifier `distribution/host/install.sh` embeds, run as pure
 * JavaScript: RFC 8032's vectors, the tampered forms it must refuse, and a
 * cross-check against `node:crypto`.
 *
 * macOS ships LibreSSL, whose `openssl(1)` has no Ed25519, so a stock Mac
 * verifies the release manifest with this text through
 * `osascript -l JavaScript`, which is JavaScriptCore. Bun and Node run the
 * same pure JavaScript, so these checks exercise the shipped source on every
 * host; `native-install.test.ts` runs it through real osascript on macOS.
 */
import { createHash, generateKeyPairSync, randomBytes, sign } from 'node:crypto'
import { expect, test } from 'bun:test'
import { ed25519JavaScriptVerifier } from './native-install-fixture.ts'

/** The verifier's own entry point, as `osascript` calls it. */
type Run = (argv: readonly string[]) => string

function verifier(): Run {
  // The shipped text is run as it ships: no import, no transform.
  return new Function(`${ed25519JavaScriptVerifier()}\nreturn run`)() as Run
}

const run = verifier()

/** The SHA-512 of R || A || M, which install.sh computes with `shasum -a 512`. */
function digest(signature: Buffer, publicKey: Buffer, message: Buffer): string {
  return createHash('sha512').update(Buffer.concat([signature.subarray(0, 32), publicKey, message])).digest('hex')
}

/** Whether the shipped verifier accepts this signature of this message. */
function verify(publicKey: Buffer, signature: Buffer, message: Buffer): boolean {
  const answer = run([publicKey.toString('hex'), signature.toString('hex'), digest(signature, publicKey, message)])
  expect(answer === 'true' || answer === 'false').toBe(true)
  return answer === 'true'
}

/** Ed25519's group order, for a signature whose S is not reduced. */
const ORDER = 2n ** 252n + 27742317777372353535851937790883648493n

function littleEndian(value: bigint, bytes: number): Buffer {
  const out = Buffer.alloc(bytes)
  let rest = value
  for (let index = 0; index < bytes; index++) {
    out[index] = Number(rest & 0xffn)
    rest >>= 8n
  }
  return out
}

function flipByte(bytes: Buffer, index: number): Buffer {
  const copy = Buffer.from(bytes)
  const current = copy[index]
  if (current === undefined) throw new Error(`No byte ${String(index)} to flip`)
  copy[index] = current ^ 0x01
  return copy
}

/** RFC 8032 section 7.1 TEST 1, TEST 2, and TEST 3. */
const VECTORS = [
  {
    publicKey: 'd75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a',
    signature: 'e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b',
    message: '',
  },
  {
    publicKey: '3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c',
    signature: '92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00',
    message: '72',
  },
  {
    publicKey: 'fc51cd8e6218a1a38da47ed00230f0580816ed13ba3303ac5deb911548908025',
    signature: '6291d657deec24024827e69c3abe01a30ce548a284743a445e3680d7db5ac3ac18ff9b538d16f290ae67f760984dc6594a7c15e9716ed28dc027beceea1ec40a',
    message: 'af82',
  },
] as const

test('accepts RFC 8032 section 7.1 vectors', () => {
  for (const vector of VECTORS) {
    const publicKey = Buffer.from(vector.publicKey, 'hex')
    const signature = Buffer.from(vector.signature, 'hex')
    expect(verify(publicKey, signature, Buffer.from(vector.message, 'hex'))).toBe(true)
  }
})

test('refuses a flipped signature, message, or key, an unreduced S, and a non-canonical point', () => {
  const vector = VECTORS[2]
  const publicKey = Buffer.from(vector.publicKey, 'hex')
  const signature = Buffer.from(vector.signature, 'hex')
  const message = Buffer.from(vector.message, 'hex')
  expect(verify(publicKey, flipByte(signature, 5), message)).toBe(false)
  expect(verify(publicKey, flipByte(signature, 40), message)).toBe(false)
  expect(verify(publicKey, signature, flipByte(message, 0))).toBe(false)
  expect(verify(flipByte(publicKey, 0), signature, message)).toBe(false)
  expect(verify(Buffer.from(VECTORS[1].publicKey, 'hex'), signature, message)).toBe(false)
  // S + L is the same scalar modulo the group order, and is refused.
  const s = BigInt(`0x${Buffer.from(signature.subarray(32)).reverse().toString('hex')}`)
  expect(verify(publicKey, Buffer.concat([signature.subarray(0, 32), littleEndian(s + ORDER, 32)]), message)).toBe(false)
  // y = p encodes no point, as R and as the key.
  const notCanonical = littleEndian(2n ** 255n - 19n, 32)
  expect(verify(publicKey, Buffer.concat([notCanonical, signature.subarray(32)]), message)).toBe(false)
  expect(verify(notCanonical, signature, message)).toBe(false)
  // Lengths the caller must not get wrong.
  expect(run([vector.publicKey.slice(0, 62), vector.signature, digest(signature, publicKey, message)])).toBe('false')
  expect(run([vector.publicKey, vector.signature.slice(0, 126), digest(signature, publicKey, message)])).toBe('false')
  expect(run([vector.publicKey, vector.signature, 'not hex'])).toBe('false')
})

test('agrees with node:crypto on 50 signatures and refuses each tampered twin', () => {
  for (let index = 0; index < 50; index++) {
    const pair = generateKeyPairSync('ed25519')
    const publicKey = pair.publicKey.export({ type: 'spki', format: 'der' }).subarray(12)
    const message = randomBytes(index === 0 ? 1023 : 1 + (index % 64))
    const signature = sign(null, message, pair.privateKey)
    expect(verify(publicKey, signature, message)).toBe(true)
    expect(verify(publicKey, signature, flipByte(message, index % message.byteLength))).toBe(false)
  }
}, 120_000)
