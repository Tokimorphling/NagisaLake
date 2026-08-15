/**
 * Small dependency-free fuzzy scorer for command-palette sized collections.
 * Lower scores are better; `null` means the query characters do not appear in
 * order. Exact substrings win, then compact subsequences and word-boundary
 * matches.
 */
export function fuzzyScore(source: string, query: string): number | null {
  const text = source.toLocaleLowerCase()
  const needle = query.trim().toLocaleLowerCase()
  if (!needle) return 0

  const exactIndex = text.indexOf(needle)
  if (exactIndex !== -1) {
    const boundaryBonus = exactIndex === 0 || /[\s:_/.-]/.test(text[exactIndex - 1]) ? -4 : 0
    return exactIndex + boundaryBonus + (text.length - needle.length) * 0.01
  }

  let cursor = 0
  let previous = -1
  let score = 20
  for (const character of needle) {
    const index = text.indexOf(character, cursor)
    if (index === -1) return null
    if (previous === -1) score += index
    else score += (index - previous - 1) * 2
    if (index === 0 || /[\s:_/.-]/.test(text[index - 1])) score -= 1.5
    previous = index
    cursor = index + 1
  }
  return score + (text.length - needle.length) * 0.02
}
