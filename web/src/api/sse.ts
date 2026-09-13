/** Incremental SSE reader for authenticated fetch streams. Never reconnects. */
export async function consumeSse(
  response: Response,
  onEvent: (eventName: string, data: string, lastEventId: string | null) => void,
  stopAfterEvent?: () => boolean,
): Promise<void> {
  if (!response.body) throw new Error('Hub returned an empty event stream')
  const reader = response.body.getReader()
  const decoder = new TextDecoder('utf-8', { fatal: true })
  const maxEventChars = 32 * 1024 * 1024
  let buffer = ''
  let scanned = 0
  let eventName = 'message'
  let eventId: string | null = null
  let data: string[] = []
  let dataChars = 0
  let skipLf = false

  const dispatch = () => {
    if (data.length > 0) onEvent(eventName, data.join('\n'), eventId)
    eventName = 'message'
    data = []
    dataChars = 0
  }
  const processLine = (line: string) => {
    if (line === '') { dispatch(); return }
    if (line.startsWith(':')) return
    const separator = line.indexOf(':')
    const field = separator === -1 ? line : line.slice(0, separator)
    let value = separator === -1 ? '' : line.slice(separator + 1)
    if (value.startsWith(' ')) value = value.slice(1)
    if (field === 'event') eventName = value
    else if (field === 'id' && !value.includes('\0')) eventId = value || null
    else if (field === 'data') {
      dataChars += value.length + 1
      if (dataChars > maxEventChars) throw new Error('Hub SSE event exceeds size limit')
      data.push(value)
    }
  }
  const feed = (text: string) => {
    buffer += text
    let start = 0
    for (let index = scanned; index < buffer.length; index++) {
      const char = buffer[index]
      if (skipLf) {
        skipLf = false
        if (char === '\n') { start = index + 1; continue }
      }
      if (char !== '\r' && char !== '\n') continue
      processLine(buffer.slice(start, index))
      if (stopAfterEvent?.()) { buffer = ''; scanned = 0; return }
      start = index + 1
      skipLf = char === '\r'
    }
    buffer = buffer.slice(start)
    scanned = buffer.length
    if (buffer.length > maxEventChars) throw new Error('Hub SSE line exceeds size limit')
  }

  try {
    while (true) {
      const { value, done } = await reader.read()
      if (done) break
      feed(decoder.decode(value, { stream: true }))
      if (stopAfterEvent?.()) return
    }
    feed(decoder.decode())
    // Incomplete events are not dispatched at EOF. Run streams require a
    // terminal event and must report truncation rather than invent completion.
  } finally {
    await reader.cancel().catch(() => undefined)
    reader.releaseLock()
  }
}
