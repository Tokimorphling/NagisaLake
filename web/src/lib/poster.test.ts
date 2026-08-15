// @vitest-environment jsdom

import { afterEach, describe, expect, it, vi } from 'vitest'
import type { PresignedRequest } from '@/api/types'
import {
  canUseDirectPosterTicket,
  POSTER_OUTPUT_WIDTH,
  PosterMediaLoadError,
  PosterTaintError,
  withPosterMediaFallback,
} from './poster'

describe('poster output defaults', () => {
  it('exports a 1080px-wide share image', () => {
    expect(POSTER_OUTPUT_WIDTH).toBe(1080)
  })
})

function ticket(overrides: Partial<PresignedRequest> = {}): PresignedRequest {
  return {
    method: 'GET',
    url: 'https://objects.example/output.mp4?signature=short-lived',
    headers: {},
    expires_at_unix_ms: Date.now() + 60_000,
    ...overrides,
  }
}

afterEach(() => {
  vi.unstubAllGlobals()
})

describe('poster media ticket fallback', () => {
  it('uses only a header-free GET ticket when direct media succeeds', async () => {
    const render = vi.fn(async (url: string) => `poster:${url}`)
    const fetchSameOrigin = vi.fn(async () => new Blob(['fallback']))

    await expect(
      withPosterMediaFallback(ticket(), render, fetchSameOrigin),
    ).resolves.toContain('https://objects.example/output.mp4')
    expect(render).toHaveBeenCalledOnce()
    expect(fetchSameOrigin).not.toHaveBeenCalled()
  })

  it.each([
    ['non-GET method', ticket({ method: 'POST' })],
    ['signed headers', ticket({ headers: { Authorization: 'signed-value' } })],
  ])('skips direct media for a ticket with %s', async (_label, mediaTicket) => {
    const createObjectURL = vi.fn(() => 'blob:same-origin-media')
    const revokeObjectURL = vi.fn()
    vi.stubGlobal('URL', { createObjectURL, revokeObjectURL })
    const render = vi.fn(async (url: string) => `poster:${url}`)
    const fetchSameOrigin = vi.fn(async () => new Blob(['fallback']))

    await expect(
      withPosterMediaFallback(mediaTicket, render, fetchSameOrigin),
    ).resolves.toBe('poster:blob:same-origin-media')
    expect(render).toHaveBeenCalledOnce()
    expect(render).toHaveBeenCalledWith('blob:same-origin-media')
    expect(fetchSameOrigin).toHaveBeenCalledOnce()
    expect(revokeObjectURL).toHaveBeenCalledWith('blob:same-origin-media')
  })

  it.each([
    new PosterMediaLoadError('CORS or expired URL'),
    new PosterTaintError(),
  ])('falls back once when direct media is inaccessible', async (directError) => {
    const createObjectURL = vi.fn(() => 'blob:same-origin-media')
    const revokeObjectURL = vi.fn()
    vi.stubGlobal('URL', { createObjectURL, revokeObjectURL })
    const attempts: string[] = []
    const render = vi.fn(async (url: string) => {
      attempts.push(url)
      if (attempts.length === 1) throw directError
      return 'fallback-poster'
    })
    const fetchSameOrigin = vi.fn(async () => new Blob(['fallback']))

    await expect(
      withPosterMediaFallback(ticket(), render, fetchSameOrigin),
    ).resolves.toBe('fallback-poster')
    expect(attempts).toEqual([
      'https://objects.example/output.mp4?signature=short-lived',
      'blob:same-origin-media',
    ])
    expect(fetchSameOrigin).toHaveBeenCalledOnce()
    expect(revokeObjectURL).toHaveBeenCalledOnce()
  })

  it('does not retry the same-origin fallback or hide unrelated render errors', async () => {
    const createObjectURL = vi.fn(() => 'blob:same-origin-media')
    const revokeObjectURL = vi.fn()
    vi.stubGlobal('URL', { createObjectURL, revokeObjectURL })
    const fallbackFailure = new PosterMediaLoadError('unsupported codec')
    const render = vi
      .fn<(url: string) => Promise<string>>()
      .mockRejectedValueOnce(new PosterMediaLoadError('direct CORS failure'))
      .mockRejectedValueOnce(fallbackFailure)
    const fetchSameOrigin = vi.fn(async () => new Blob(['fallback']))

    await expect(
      withPosterMediaFallback(ticket(), render, fetchSameOrigin),
    ).rejects.toBe(fallbackFailure)
    expect(render).toHaveBeenCalledTimes(2)
    expect(fetchSameOrigin).toHaveBeenCalledOnce()

    const unrelated = new Error('Canvas 2D unavailable')
    const unrelatedRender = vi.fn(async () => {
      throw unrelated
    })
    const unrelatedFallback = vi.fn(async () => new Blob(['unused']))
    await expect(
      withPosterMediaFallback(ticket(), unrelatedRender, unrelatedFallback),
    ).rejects.toBe(unrelated)
    expect(unrelatedFallback).not.toHaveBeenCalled()
  })

  it('accepts GET case-insensitively but rejects unsafe or expiring tickets', () => {
    expect(canUseDirectPosterTicket(ticket({ method: 'get' }))).toBe(true)
    expect(canUseDirectPosterTicket(ticket({ method: 'HEAD' }))).toBe(false)
    expect(canUseDirectPosterTicket(ticket({ headers: { 'X-Empty': '' } }))).toBe(false)
    expect(
      canUseDirectPosterTicket(ticket({ expires_at_unix_ms: Date.now() + 1_000 })),
    ).toBe(false)
  })
})
