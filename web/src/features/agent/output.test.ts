import { describe, expect, it } from 'vitest'
import { agentSections } from './output'

describe('Agent plaintext section rendering', () => {
  it('understands H3 headings without changing the original output contract', () => {
    expect(agentSections('**integrated_multimodal_description**\nA quiet lake.\n\n**overall_soundscape**\nWind.')).toEqual([
      {title:'integrated_multimodal_description',text:'A quiet lake.'}, {title:'overall_soundscape',text:'Wind.'},
    ])
  })
  it('retains leading prose and treats markup as text, never HTML', () => {
    expect(agentSections('Introduction\n## Description\n<script>alert(1)</script>')).toEqual([
      {title:null,text:'Introduction'}, {title:'Description',text:'<script>alert(1)</script>'},
    ])
    expect(agentSections('A simple prompt.')).toEqual([{title:null,text:'A simple prompt.'}])
  })
})
