export interface GalleryPromptSection {
  label: string | null
  value: string
}

// Accept both common Markdown label forms: **Title:** and **Title**:.
// A deliberately small label grammar prevents ordinary bold prose from
// unexpectedly becoming a section heading.
const SECTION_MARKER =
  /\*\*\s*([^*:\r\n]{1,64}?)\s*:\s*\*\*|\*\*\s*([^*:\r\n]{1,64}?)\s*\*\*\s*:/gu

function normalizeLabel(value: string): string {
  return value.trim().replace(/\s+/gu, ' ')
}

/**
 * Turns a prompt containing Markdown-like labelled blocks into readable
 * sections without losing any of the prompt text.
 */
export function parseGalleryPrompt(prompt: string): GalleryPromptSection[] {
  const text = prompt.trim()
  if (!text) return []

  const markers = [...text.matchAll(SECTION_MARKER)]
  if (markers.length === 0) return [{ label: null, value: text }]

  const sections: GalleryPromptSection[] = []
  const leadingText = text.slice(0, markers[0].index).trim()
  if (leadingText) sections.push({ label: null, value: leadingText })

  for (const [index, marker] of markers.entries()) {
    const valueStart = (marker.index ?? 0) + marker[0].length
    const valueEnd = markers[index + 1]?.index ?? text.length
    const value = text.slice(valueStart, valueEnd).trim()
    if (!value) continue

    sections.push({
      label: normalizeLabel(marker[1] ?? marker[2] ?? ''),
      value,
    })
  }

  return sections
}
