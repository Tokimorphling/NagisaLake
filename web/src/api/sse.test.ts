import { describe, expect, it } from 'vitest'
import { consumeSse } from './sse'

function bytesResponse(text: string, chunkSize = 1) {
  const bytes = new TextEncoder().encode(text)
  return new Response(new ReadableStream<Uint8Array>({ start(controller) {
    for (let offset = 0; offset < bytes.length; offset += chunkSize) controller.enqueue(bytes.slice(offset, offset + chunkSize))
    controller.close()
  } }))
}

describe('SSE framing', () => {
  it('handles split UTF-8 and CRLF, multiline data and persistent ids', async () => {
    const values: unknown[] = []
    await consumeSse(bytesResponse(': ping\r\nid: 7\r\nevent: text_delta\r\ndata: 你好\r\ndata: world\r\n\r\ndata: next\r\r'), (...event) => values.push(event))
    expect(values).toEqual([['text_delta', '你好\nworld', '7'], ['message', 'next', '7']])
  })
  it('does not turn an incomplete frame into an event at EOF', async () => {
    const values: string[] = []
    await consumeSse(bytesResponse('data: partial\n'), (_name, data) => values.push(data))
    expect(values).toEqual([])
  })
  it('releases the body reader even when the consumer throws', async () => {
    const response = bytesResponse('data: invalid\n\n', 20)
    await expect(consumeSse(response, () => { throw new Error('bad event') })).rejects.toThrow('bad event')
    expect(response.body?.locked).toBe(false)
  })
})
