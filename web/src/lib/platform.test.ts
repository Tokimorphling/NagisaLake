// The clipboard fallback is DOM work — it appends a scratch textarea and must
// remove it again — so this file needs a document.
// @vitest-environment jsdom

import { afterEach, describe, expect, it, vi } from 'vitest'
import { copyText, randomUuid } from './platform'

afterEach(() => {
  vi.unstubAllGlobals()
  vi.restoreAllMocks()
})

const UUID_V4 = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/

describe('randomUuid', () => {
  it('uses crypto.randomUUID when it is available', () => {
    const randomUUID = vi.fn(() => '11111111-2222-4333-8444-555555555555')
    vi.stubGlobal('crypto', { ...globalThis.crypto, randomUUID })

    expect(randomUuid()).toBe('11111111-2222-4333-8444-555555555555')
    expect(randomUUID).toHaveBeenCalledOnce()
  })

  // A plain-HTTP LAN origin has getRandomValues but not randomUUID.
  it('falls back to getRandomValues and still produces a v4 UUID', () => {
    const getRandomValues = vi.fn((buffer: Uint8Array) => {
      buffer.fill(0xff)
      return buffer
    })
    vi.stubGlobal('crypto', { getRandomValues })

    const value = randomUuid()
    expect(value).toMatch(UUID_V4)
    expect(getRandomValues).toHaveBeenCalledOnce()
    // Version and variant bits must be forced even when every byte is 0xff.
    expect(value[14]).toBe('4')
    expect(['8', '9', 'a', 'b']).toContain(value[19])
  })

  it('still returns a usable value with no crypto at all', () => {
    vi.stubGlobal('crypto', undefined)
    expect(randomUuid()).toMatch(UUID_V4)
  })

  // The value is an idempotency key: a collision would make the Hub replay an
  // earlier job instead of submitting a new one.
  it('does not repeat across many calls', () => {
    const getRandomValues = (buffer: Uint8Array) => {
      for (let index = 0; index < buffer.length; index += 1) {
        buffer[index] = Math.floor(Math.random() * 256)
      }
      return buffer
    }
    vi.stubGlobal('crypto', { getRandomValues })

    const values = new Set(Array.from({ length: 2000 }, () => randomUuid()))
    expect(values.size).toBe(2000)
  })
})

describe('copyText', () => {
  it('prefers the async clipboard API', async () => {
    const writeText = vi.fn(async () => undefined)
    vi.stubGlobal('navigator', { ...globalThis.navigator, clipboard: { writeText } })

    await expect(copyText('nsk_secret')).resolves.toBe(true)
    expect(writeText).toHaveBeenCalledWith('nsk_secret')
  })

  // navigator.clipboard is unavailable on a plain-HTTP LAN origin.
  it('falls back to execCommand when the clipboard API is missing', async () => {
    vi.stubGlobal('navigator', { ...globalThis.navigator, clipboard: undefined })
    const execCommand = vi.fn(() => true)
    Object.defineProperty(document, 'execCommand', {
      value: execCommand,
      configurable: true,
      writable: true,
    })

    await expect(copyText('fallback')).resolves.toBe(true)
    expect(execCommand).toHaveBeenCalledWith('copy')
    // The scratch textarea must not be left behind in the DOM.
    expect(document.querySelector('textarea')).toBeNull()
  })

  it('reports failure rather than claiming a copy that did not happen', async () => {
    vi.stubGlobal('navigator', { ...globalThis.navigator, clipboard: undefined })
    Object.defineProperty(document, 'execCommand', {
      value: vi.fn(() => false),
      configurable: true,
      writable: true,
    })

    await expect(copyText('nope')).resolves.toBe(false)
    expect(document.querySelector('textarea')).toBeNull()
  })

  it('falls back when the clipboard API rejects', async () => {
    vi.stubGlobal('navigator', {
      ...globalThis.navigator,
      clipboard: {
        writeText: vi.fn(async () => {
          throw new Error('document is not focused')
        }),
      },
    })
    const execCommand = vi.fn(() => true)
    Object.defineProperty(document, 'execCommand', {
      value: execCommand,
      configurable: true,
      writable: true,
    })

    await expect(copyText('retry')).resolves.toBe(true)
    expect(execCommand).toHaveBeenCalledWith('copy')
  })
})
