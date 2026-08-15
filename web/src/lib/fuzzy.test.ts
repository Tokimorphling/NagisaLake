import { describe, expect, it } from 'vitest'
import { fuzzyScore } from './fuzzy'

describe('fuzzyScore', () => {
  it('matches ordered subsequences and rejects reordered characters', () => {
    expect(fuzzyScore('Workflow 目录', 'wfl')).not.toBeNull()
    expect(fuzzyScore('Workflow 目录', 'zxy')).toBeNull()
  })

  it('ranks exact and compact matches ahead of loose subsequences', () => {
    const exact = fuzzyScore('job abc123', 'abc') as number
    const loose = fuzzyScore('a long break before b and c', 'abc') as number
    expect(exact).toBeLessThan(loose)
  })
})
