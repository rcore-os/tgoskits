//! 生命周期收口的确定性单测。
//!
//! 断言纪律与 `http_probe.py` 一致：200 不代表终态，要靠轮询详情里的计数增长
//! 或状态到达来证明。这里用注入的假时钟把「何时算收口、何时算超时」钉死。

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

/** 假时钟：每次 sleep 前进一个周期，并在超时前让序列走完。 */
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

/** 依次返回给定观测序列，用尽后重复最后一个（模拟状态不再变化）。 */
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
  it('start 只有在 vCPU 真正进入 guest 后才算收口', () => {
    expect(isSettled('start', BEFORE, detail('running', 0, 0))).toBe(false)
    expect(isSettled('start', BEFORE, detail('running', 1, 0))).toBe(true)
  })

  it('pause 只有在 vCPU 真正 park 后才算收口', () => {
    expect(isSettled('pause', BEFORE, detail('paused', 0, 0))).toBe(false)
    expect(isSettled('pause', BEFORE, detail('paused', 0, 1))).toBe(true)
  })

  it('resume 与 start 同判据', () => {
    expect(isSettled('resume', BEFORE, detail('running', 0, 0))).toBe(false)
    expect(isSettled('resume', BEFORE, detail('running', 1, 0))).toBe(true)
  })

  it('stop 只需到达 stopped（异步，vCPU 退出后才成立）', () => {
    expect(isSettled('stop', BEFORE, detail('stopping'))).toBe(false)
    expect(isSettled('stop', BEFORE, detail('stopped'))).toBe(true)
  })

  it('create 只需到达 ready', () => {
    expect(isSettled('create', BEFORE, detail('created'))).toBe(false)
    expect(isSettled('create', BEFORE, detail('ready'))).toBe(true)
  })

  it('delete 没有可断言的状态，由 404 判定', () => {
    expect(isSettled('delete', BEFORE, detail('stopped'))).toBe(false)
  })
})

describe('settleToTerminalState', () => {
  it('轮询到计数增长为止', async () => {
    const result = await settle('start', [detail('running', 0), detail('running', 1)])
    expect(result.ok).toBe(true)
  })

  it('计数不增长时超时，并保留最后一次观测', async () => {
    const result = await settle('pause', [detail('paused', 0, 0)])
    expect(result.ok).toBe(false)
    if (result.ok) return
    expect(result.detail?.status).toBe('paused')
    expect(result.message).toContain('guest_park_count')
    expect(result.message).toContain('park=0')
  })

  it('delete 遇到 404 视为成功', async () => {
    const { deps } = fakeClock()
    const fetchDetail = async () => {
      throw new ApiError(404, '')
    }
    expect((await settleToTerminalState('delete', BEFORE, fetchDetail, deps)).ok).toBe(true)
  })

  it('delete 的 404 之外的错误继续轮询到超时', async () => {
    const { deps } = fakeClock()
    const fetchDetail = async () => {
      throw new ApiError(500, '')
    }
    const result = await settleToTerminalState('delete', BEFORE, fetchDetail, deps)
    expect(result.ok).toBe(false)
  })

  it('基线非零时按增量判断，而不是按绝对值为零', () => {
    const before: Counters = { guest_entry_count: 7, guest_park_count: 0 }
    expect(isSettled('start', before, detail('running', 7, 0))).toBe(false)
    expect(isSettled('start', before, detail('running', 8, 0))).toBe(true)
  })
})

describe('countersOf', () => {
  it('缺失字段按 0 处理', () => {
    expect(countersOf({ id: 1, status: 'ready' })).toEqual(BEFORE)
  })
})

describe('展示降级', () => {
  it('未知状态原样显示而不是崩', () => {
    expect(describeStatus('running')).toBe('运行中')
    expect(describeStatus('some-future-state')).toBe('some-future-state')
  })

  it('空错误体按状态码给出可读文案', () => {
    expect(describeError(new ApiError(409, ''))).toBe('HTTP 409 · 当前状态不允许该操作')
    expect(describeError(new ApiError(401, ''))).toContain('AXVM_HTTP_TOKEN')
    expect(describeError(new ApiError(503, ''))).toBe('HTTP 503 · 宿主资源不足')
  })
})
