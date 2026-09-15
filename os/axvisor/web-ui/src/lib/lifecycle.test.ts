//! Deterministic unit tests for lifecycle settling.
//!
//! The assertion discipline matches `http_probe.py`: a 200 does not mean the terminal
//! state was reached — it has to be proven by a growing counter or a settled status in
//! the polled detail. An injected fake clock pins down what counts as settled and what
//! counts as a timeout.

import { describe, expect, it } from 'vitest'
import { ApiError, describeError, describeStatus, type VmDetail } from '@/api/types'
import {
  countersOf,
  isSettled,
  settleToTerminalState,
  type Counters,
  type LifecycleOp,
} from '@/lib/lifecycle'

const BEFORE: Counters = { guest_entry_count: 0, guest_park_count: 0 }

function detail(status: string, entry = 0, park = 0): VmDetail {
  return { id: 1, status, guest_entry_count: entry, guest_park_count: park }
}

/** Fake clock: every sleep advances one interval, letting the sequence run out before the timeout. */
function fakeClock(intervalMs = 100, timeoutMs = 1000) {
  let now = 0
  return {
    deps: {
      now: () => now,
      sleep: async () => {
        now += intervalMs
      },
      intervalMs,
      timeoutMs,
    },
  }
}

/** Yields the given observation sequence, then repeats the last one (simulating a status that stops changing). */
function sequenceFetcher(observations: VmDetail[]) {
  let index = 0
  return async () => {
    const observation = observations[Math.min(index, observations.length - 1)]
    index += 1
    return observation
  }
}

async function settle(op: LifecycleOp, observations: VmDetail[], before = BEFORE) {
  const { deps } = fakeClock()
  return settleToTerminalState(op, before, sequenceFetcher(observations), deps)
}

describe('isSettled', () => {
  it('start settles only after a vCPU really entered the guest', () => {
    expect(isSettled('start', BEFORE, detail('running', 0, 0))).toBe(false)
    expect(isSettled('start', BEFORE, detail('running', 1, 0))).toBe(true)
  })

  it('pause settles only after a vCPU really parked', () => {
    expect(isSettled('pause', BEFORE, detail('paused', 0, 0))).toBe(false)
    expect(isSettled('pause', BEFORE, detail('paused', 0, 1))).toBe(true)
  })

  it('resume shares the start predicate', () => {
    expect(isSettled('resume', BEFORE, detail('running', 0, 0))).toBe(false)
    expect(isSettled('resume', BEFORE, detail('running', 1, 0))).toBe(true)
  })

  it('stop only needs to reach stopped (async, holds once the vCPU exits)', () => {
    expect(isSettled('stop', BEFORE, detail('stopping'))).toBe(false)
    expect(isSettled('stop', BEFORE, detail('stopped'))).toBe(true)
  })

  it('create only needs to reach ready', () => {
    expect(isSettled('create', BEFORE, detail('created'))).toBe(false)
    expect(isSettled('create', BEFORE, detail('ready'))).toBe(true)
  })

  it('delete has no assertable status; a 404 decides it', () => {
    expect(isSettled('delete', BEFORE, detail('stopped'))).toBe(false)
  })
})

describe('settleToTerminalState', () => {
  it('polls until the counter grows', async () => {
    const result = await settle('start', [detail('running', 0), detail('running', 1)])
    expect(result.ok).toBe(true)
  })

  it('times out when the counter does not grow, keeping the last observation', async () => {
    const result = await settle('pause', [detail('paused', 0, 0)])
    expect(result.ok).toBe(false)
    if (result.ok) return
    expect(result.detail?.status).toBe('paused')
    expect(result.message).toContain('guest_park_count')
    expect(result.message).toContain('park=0')
  })

  it('delete treats a 404 as success', async () => {
    const { deps } = fakeClock()
    const fetchDetail = async () => {
      throw new ApiError(404, '')
    }
    expect((await settleToTerminalState('delete', BEFORE, fetchDetail, deps)).ok).toBe(true)
  })

  it('errors other than 404 keep delete polling until the timeout', async () => {
    const { deps } = fakeClock()
    const fetchDetail = async () => {
      throw new ApiError(500, '')
    }
    const result = await settleToTerminalState('delete', BEFORE, fetchDetail, deps)
    expect(result.ok).toBe(false)
  })

  // Regression: without a per-request signal the loop awaited a request that
  // never settles and the deadline below was never reached, so the poll hung
  // forever and neither the timeout message nor the caller's `busy` reset ran.
  it('a detail request that never settles is aborted instead of hanging the poll', async () => {
    const { deps } = fakeClock(1, 1)
    const neverSettles = (signal?: AbortSignal) =>
      new Promise<VmDetail>((_resolve, reject) => {
        signal?.addEventListener('abort', () => reject(new Error('aborted at the deadline')))
      })
    const result = await settleToTerminalState('stop', BEFORE, neverSettles, deps)
    expect(result.ok).toBe(false)
  })

  it('a non-zero baseline is judged by the delta, not by an absolute zero', () => {
    const before: Counters = { guest_entry_count: 7, guest_park_count: 0 }
    expect(isSettled('start', before, detail('running', 7, 0))).toBe(false)
    expect(isSettled('start', before, detail('running', 8, 0))).toBe(true)
  })
})

describe('countersOf', () => {
  it('missing fields are treated as 0', () => {
    expect(countersOf({ id: 1, status: 'ready' })).toEqual(BEFORE)
  })
})

describe('display degradation', () => {
  it('an unknown status is shown verbatim instead of crashing', () => {
    expect(describeStatus('running')).toBe('运行中')
    expect(describeStatus('some-future-state')).toBe('some-future-state')
  })

  it('an empty error body yields readable text from the status code', () => {
    expect(describeError(new ApiError(409, ''))).toBe('HTTP 409 · 当前状态不允许该操作')
    expect(describeError(new ApiError(401, ''))).toContain('AXVM_HTTP_TOKEN')
    expect(describeError(new ApiError(503, ''))).toBe('HTTP 503 · 宿主资源不足')
  })
})
