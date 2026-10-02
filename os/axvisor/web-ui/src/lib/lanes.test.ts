import { describe, expect, it } from 'vitest'
import {
  guestRoute,
  laneRefused,
  laneVmId,
  lanesToOpen,
  mergeGroups,
  nextFreeTab,
  releaseLane,
  sameSet,
  splitGroup,
  syncGroups,
} from './lanes'

describe('lane names', () => {
  it('derives a guest lane from a VM id', () => {
    expect(guestRoute(1)).toBe('vm-1')
    expect(guestRoute(42)).toBe('vm-42')
  })

  it('recovers the VM id of a guest lane', () => {
    expect(laneVmId('vm-1')).toBe(1)
    expect(laneVmId('vm-42')).toBe(42)
  })

  it('reports no VM for the management lane or an unknown shape', () => {
    expect(laneVmId('axvisor')).toBeNull()
    expect(laneVmId('vm-')).toBeNull()
    expect(laneVmId('vm-1x')).toBeNull()
    expect(laneVmId('vmcache')).toBeNull()
  })

  it('round-trips every lane it names', () => {
    for (const id of [1, 2, 7, 8]) {
      expect(laneVmId(guestRoute(id))).toBe(id)
    }
  })
})

describe('open lanes', () => {
  it('does not mutate the set it was given', () => {
    const open = ['vm-1']
    releaseLane(open, 'vm-1')
    expect(open).toEqual(['vm-1'])
  })

  it('releases only the named lane', () => {
    expect(releaseLane(['vm-1', 'vm-2'], 'vm-1')).toEqual(['vm-2'])
    expect(releaseLane(['vm-1'], 'vm-3')).toEqual(['vm-1'])
  })

  it('opens the lane of the tab the operator is looking at', () => {
    const lanes = [
      { route: 'vm-1', attached: false },
      { route: 'vm-2', attached: false },
    ]
    expect(lanesToOpen(['vm-1'], lanes)).toEqual(['vm-1'])
  })

  it('never opens a free lane that belongs to another tab', () => {
    const lanes = [
      { route: 'vm-1', attached: true },
      { route: 'vm-2', attached: false },
      { route: 'vm-3', attached: false },
    ]
    // vm-1 is held, so this tab opens nothing even though vm-2 and vm-3 are
    // free: connecting a lane the operator is not looking at is what merged a
    // foreign terminal into the tab on screen.
    expect(lanesToOpen(['vm-1'], lanes)).toEqual([])
  })

  it('opens nothing when every lane of the tab is held by a session', () => {
    const lanes = [
      { route: 'vm-1', attached: true },
      { route: 'vm-2', attached: true },
    ]
    expect(lanesToOpen(['vm-1', 'vm-2'], lanes)).toEqual([])
  })

  it('skips the lanes of a merged tab that this page lost or released', () => {
    const lanes = [
      { route: 'vm-1', attached: false },
      { route: 'vm-2', attached: false },
      { route: 'vm-3', attached: false },
    ]
    expect(lanesToOpen(['vm-1', 'vm-2', 'vm-3'], lanes, ['vm-2', 'vm-3'])).toEqual(['vm-1'])
  })

  it('moves to a tab that can still be opened after losing a race', () => {
    const groups = [['vm-1'], ['vm-2'], ['vm-3']]
    const lanes = [
      { route: 'vm-1', attached: true },
      { route: 'vm-2', attached: false },
      { route: 'vm-3', attached: false },
    ]
    expect(nextFreeTab(groups, lanes)).toBe(1)
  })

  it('finds no tab to move to when every lane is held or already lost', () => {
    const groups = [['vm-1'], ['vm-2'], ['vm-3']]
    const lanes = [
      { route: 'vm-1', attached: true },
      { route: 'vm-2', attached: false },
      { route: 'vm-3', attached: true },
    ]
    expect(nextFreeTab(groups, lanes, ['vm-2'])).toBe(-1)
  })

  it('tells a lost race apart from a backend that went away', () => {
    const held = [{ route: 'vm-1', attached: true }]
    const free = [{ route: 'vm-1', attached: false }]
    expect(laneRefused(held, 'vm-1')).toBe(true)
    expect(laneRefused(free, 'vm-1')).toBe(false)
    expect(laneRefused(held, 'vm-2')).toBe(false)
  })

  it('compares lane sets without regard to order', () => {
    expect(sameSet(['vm-1', 'vm-2'], ['vm-2', 'vm-1'])).toBe(true)
    expect(sameSet(['vm-1'], ['vm-1', 'vm-2'])).toBe(false)
    expect(sameSet([], [])).toBe(true)
  })
})

describe('tab groups', () => {
  it('gives every console its own tab until they are merged', () => {
    expect(syncGroups([], ['vm-1', 'vm-2'])).toEqual([['vm-1'], ['vm-2']])
  })

  it('keeps a merged tab together across a lane table change', () => {
    expect(syncGroups([['vm-1', 'vm-2']], ['vm-1', 'vm-2', 'vm-3'])).toEqual([
      ['vm-1', 'vm-2'],
      ['vm-3'],
    ])
  })

  it('drops a lane whose VM is gone from the tab that held it', () => {
    expect(syncGroups([['vm-1', 'vm-2'], ['vm-3']], ['vm-2', 'vm-3'])).toEqual([
      ['vm-2'],
      ['vm-3'],
    ])
    expect(syncGroups([['vm-1']], ['vm-2'])).toEqual([['vm-2']])
  })

  it('merges the dropped tab into the tab it landed on', () => {
    expect(mergeGroups([['vm-1'], ['vm-2']], 1, 0)).toEqual([['vm-1', 'vm-2']])
    expect(mergeGroups([['vm-3'], ['vm-1'], ['vm-2']], 0, 2)).toEqual([
      ['vm-1'],
      ['vm-2', 'vm-3'],
    ])
  })

  it('never lists a lane twice when merging', () => {
    expect(mergeGroups([['vm-1', 'vm-2'], ['vm-2']], 1, 0)).toEqual([['vm-1', 'vm-2']])
    expect(mergeGroups([['vm-1']], 0, 0)).toEqual([['vm-1']])
    expect(mergeGroups([['vm-1']], 5, 0)).toEqual([['vm-1']])
  })

  it('splits a lane back out into its own tab at the end', () => {
    expect(splitGroup([['vm-1', 'vm-2'], ['vm-3']], 'vm-2')).toEqual([
      ['vm-1'],
      ['vm-3'],
      ['vm-2'],
    ])
    expect(splitGroup([['vm-1', 'vm-2']], 'vm-1')).toEqual([['vm-2'], ['vm-1']])
    expect(splitGroup([['vm-1']], 'vm-9')).toEqual([['vm-1']])
  })
})
