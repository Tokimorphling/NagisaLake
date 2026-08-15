import { describe, expect, it } from 'vitest'
import { parseGalleryPrompt } from './gallery-prompt'

describe('parseGalleryPrompt', () => {
  it('structures adjacent bold labels while preserving the complete values', () => {
    const prompt =
      '**Title:** Over-the-shoulder portrait 3: Station escalator **Identity Lock:** None. **Style:** Surreal photorealistic. Real iPhone photography. **Negative / Limitations:** No CGI. No anime.'

    expect(parseGalleryPrompt(prompt)).toEqual([
      { label: 'Title', value: 'Over-the-shoulder portrait 3: Station escalator' },
      { label: 'Identity Lock', value: 'None.' },
      { label: 'Style', value: 'Surreal photorealistic. Real iPhone photography.' },
      { label: 'Negative / Limitations', value: 'No CGI. No anime.' },
    ])
  })

  it('accepts a colon outside the bold marker and normalizes label whitespace', () => {
    expect(parseGalleryPrompt('**Camera angle**: Low angle\n**  Lighting  setup  **: Soft light')).toEqual([
      { label: 'Camera angle', value: 'Low angle' },
      { label: 'Lighting setup', value: 'Soft light' },
    ])
  })

  it('keeps unlabelled text before the first section and all multiline content', () => {
    expect(parseGalleryPrompt('Portrait baseline.\n\n**Pose:** Looking back\nwith a soft smile.')).toEqual([
      { label: null, value: 'Portrait baseline.' },
      { label: 'Pose', value: 'Looking back\nwith a soft smile.' },
    ])
  })

  it('returns an ordinary prompt as one unlabelled section', () => {
    expect(parseGalleryPrompt('A quiet lake at dawn, **soft light** and mist.')).toEqual([
      { label: null, value: 'A quiet lake at dawn, **soft light** and mist.' },
    ])
  })

  it('returns no sections for a blank prompt', () => {
    expect(parseGalleryPrompt(' \n ')).toEqual([])
  })
})
