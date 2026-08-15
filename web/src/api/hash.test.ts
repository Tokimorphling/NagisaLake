import { createHash } from 'node:crypto'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { sha256Hex } from './hash'

afterEach(() => {
  vi.unstubAllGlobals()
})

describe('sha256Hex', () => {
  it('uses WebCrypto when it is available', async () => {
    await expect(sha256Hex(new Blob(['abc']))).resolves.toBe(
      'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad',
    )
  })

  it('uses the incremental fallback when WebCrypto is unavailable', async () => {
    vi.stubGlobal('crypto', undefined)

    await expect(sha256Hex(new Blob(['abc']))).resolves.toBe(
      'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad',
    )
  })

  it('reads fallback input in chunks instead of buffering the entire blob', async () => {
    const bytes = new Uint8Array(4 * 1024 * 1024 + 17).fill(0xa5)
    const expected = createHash('sha256').update(bytes).digest('hex')
    const file = new Blob([bytes])
    const wholeFileRead = vi.spyOn(file, 'arrayBuffer')
    vi.stubGlobal('crypto', undefined)

    await expect(sha256Hex(file)).resolves.toBe(expected)
    expect(wholeFileRead).not.toHaveBeenCalled()
  })
})
