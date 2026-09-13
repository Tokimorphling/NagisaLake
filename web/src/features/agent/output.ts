export interface AgentSection { title: string | null; text: string }

/** A small plaintext renderer, not arbitrary HTML/Markdown from a provider. */
export function agentSections(text: string): AgentSection[] {
  const headings = [...text.matchAll(/^(?:\*\*([^*\r\n]{1,80})\*\*:?[ \t]*|#{1,3}[ \t]+([^\r\n]{1,80}))[ \t]*$/gm)]
  if (!headings.length) return [{ title: null, text }]
  const sections: AgentSection[] = []
  const leading = text.slice(0, headings[0].index).trim()
  if (leading) sections.push({ title: null, text: leading })
  for (const [index, heading] of headings.entries()) {
    const content = text.slice((heading.index ?? 0) + heading[0].length, headings[index + 1]?.index ?? text.length).trim()
    if (content) sections.push({ title: (heading[1] ?? heading[2]).replace(/:$/, '').trim(), text: content })
  }
  return sections
}
