import { describe, expect, it } from 'vitest'
import { decodeCpuSet, describeCpuAffinity } from './vcpu'

// `phys_cpu_set` is a bitmask, which is what these cases pin down: the panel
// crashed on `mask.join(',')` before the contract was fixed, and a 32-bit
// bitwise decode would lose the upper half of a host CPU mask.
describe('decodeCpuSet', () => {
  it('treats a missing mask as "not pinned"', () => {
    expect(decodeCpuSet(null)).toEqual([])
    expect(decodeCpuSet(undefined)).toEqual([])
    expect(decodeCpuSet(0)).toEqual([])
  })

  it('reads bit n as Core n', () => {
    expect(decodeCpuSet(0b1)).toEqual([0])
    expect(decodeCpuSet(0b10)).toEqual([1])
    expect(decodeCpuSet(0b1011)).toEqual([0, 1, 3])
  })

  it('keeps bits above 31', () => {
    expect(decodeCpuSet(2 ** 32)).toEqual([32])
    expect(decodeCpuSet(2 ** 40 + 0b10)).toEqual([1, 40])
  })
})

describe('describeCpuAffinity', () => {
  it('names the pinned cores and keeps the raw mask for the log', () => {
    expect(describeCpuAffinity(0b10)).toBe('Core 1（掩码 0x2）')
    expect(describeCpuAffinity(0b1011)).toBe('Core 0,1,3（掩码 0xb）')
  })

  it('has a word for an unpinned vCPU', () => {
    expect(describeCpuAffinity(null)).toBe('未绑定')
  })
})
