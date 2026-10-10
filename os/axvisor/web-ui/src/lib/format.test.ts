import { describe, expect, it } from 'vitest'
import { formatMemory, formatUptime, overcommit, ratio } from './format'

// The panels show these numbers next to each other, so what the tests pin down
// is the unit they agree on: an uptime that flips between `1h` and `01:00:00`
// between two refreshes, or a memory figure that crosses the GiB line and
// changes unit, is a dashboard that looks like it is flickering.
describe('formatUptime', () => {
  it('leads with the unit that is not zero', () => {
    expect(formatUptime(0)).toBe('00:00:00')
    expect(formatUptime(9)).toBe('00:00:09')
    expect(formatUptime(125)).toBe('00:02:05')
    expect(formatUptime(3600 * 2 + 125)).toBe('02:02:05')
    expect(formatUptime(86400 * 3 + 3600 * 4 + 300)).toBe('3d 04:05')
  })

  it('has a placeholder for a figure the backend did not send', () => {
    expect(formatUptime(Number.NaN)).toBe('—')
    expect(formatUptime(-1)).toBe('—')
  })
})

describe('formatMemory', () => {
  it('switches to GiB at 1024 MiB and not before', () => {
    expect(formatMemory(512)).toBe('512 MiB')
    expect(formatMemory(1023)).toBe('1023 MiB')
    expect(formatMemory(1024)).toBe('1.0 GiB')
    expect(formatMemory(6144)).toBe('6.0 GiB')
  })
})

describe('ratio', () => {
  it('clamps an overcommitted allocation to a full bar', () => {
    expect(ratio(6, 8)).toBe(0.75)
    expect(ratio(12, 8)).toBe(1)
    expect(ratio(1, 0)).toBe(0)
  })
})

describe('overcommit', () => {
  it('reports how many times over the total the allocation is', () => {
    expect(overcommit(12, 8)).toBe(1.5)
    expect(overcommit(8, 8)).toBe(1)
    expect(overcommit(3, 0)).toBe(0)
  })
})
