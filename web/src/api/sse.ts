/** Minimal SSE reader for authenticated fetch streams. */
export async function consumeSse(
  response: Response,
  onEvent: (eventName: string, data: string, lastEventId: string | null) => void,
): Promise<void> {
  if (!response.body) throw new Error('Hub returned an empty event stream')

  const reader = response.body.getReader()
  const decoder = new TextDecoder()
  let buffer = ''
  let eventName = 'message'
  let eventId: string | null = null
  let data: string[] = []

  const dispatch = () => {
    if (data.length === 0) return
    onEvent(eventName, data.join('\n'), eventId)
    eventName = 'message'
    eventId = null
    data = []
  }

  const processLine = (line: string) => {
    if (line === '') {
      dispatch()
      return
    }
    if (line.startsWith(':')) return

    const separator = line.indexOf(':')
    const field = separator === -1 ? line : line.slice(0, separator)
    let value = separator === -1 ? '' : line.slice(separator + 1)
    if (value.startsWith(' ')) value = value.slice(1)

    if (field === 'event') eventName = value
    else if (field === 'id' && !value.includes('\0')) eventId = value
    else if (field === 'data') data.push(value)
  }

  while (true) {
    const { value, done } = await reader.read()
    if (done) break
    buffer += decoder.decode(value, { stream: true })

    let newline = buffer.indexOf('\n')
    while (newline !== -1) {
      let line = buffer.slice(0, newline)
      buffer = buffer.slice(newline + 1)
      if (line.endsWith('\r')) line = line.slice(0, -1)
      processLine(line)
      newline = buffer.indexOf('\n')
    }
  }

  buffer += decoder.decode()
  if (buffer.length > 0) processLine(buffer.endsWith('\r') ? buffer.slice(0, -1) : buffer)
  dispatch()
}
