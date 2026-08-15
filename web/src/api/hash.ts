import { sha256 } from '@noble/hashes/sha2.js'

const HASH_CHUNK_BYTES = 4 * 1024 * 1024

function bytesToHex(bytes: Uint8Array): string {
  return Array.from(bytes)
    .map((byte) => byte.toString(16).padStart(2, '0'))
    .join('')
}

/** Incremental fallback for insecure LAN HTTP origins where WebCrypto is unavailable. */
async function sha256HexIncremental(file: Blob): Promise<string> {
  const hash = sha256.create()
  for (let offset = 0; offset < file.size; offset += HASH_CHUNK_BYTES) {
    const chunk = file.slice(offset, offset + HASH_CHUNK_BYTES)
    hash.update(new Uint8Array(await chunk.arrayBuffer()))
  }
  return bytesToHex(hash.digest())
}

/** Hex SHA-256, matching the 64-char lowercase digest the Hub validates. */
export async function sha256Hex(file: Blob): Promise<string> {
  const subtle = globalThis.crypto?.subtle
  if (!subtle) return sha256HexIncremental(file)

  const digest = await subtle.digest('SHA-256', await file.arrayBuffer())
  return bytesToHex(new Uint8Array(digest))
}
