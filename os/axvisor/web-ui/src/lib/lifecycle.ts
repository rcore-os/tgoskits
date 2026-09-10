//! 生命周期收口：**HTTP 200 只代表请求被接受，不等于到达终态**。
//!
//! 每个变更操作完成后轮询 VM 详情，直到断言成立或超时。断言纪律与
//! `test-suit/axvisor/normal/qemu-http-control-plane` 的 `http_probe.py` 一致：
//! start/resume 要 `guest_entry_count` 严格增长（vCPU 真正进入 guest），
//! pause 要 `guest_park_count` 严格增长（vCPU 真正 park，不只是状态翻转），
//! stop 要 `stopped`，create 要 `ready`，delete 要详情变 404。
//!
//! 全部是纯函数 + 注入依赖，便于用 vitest 做确定性单测。

import { ApiError, type VmDetail } from '@/api/types'

export type LifecycleOp = 'create' | 'start' | 'pause' | 'resume' | 'stop' | 'delete'

/** 操作前的计数基线：只有需要「严格增长」证明的操作才会用到。 */
export interface Counters {
  guest_entry_count: number
  guest_park_count: number
}

export const POLL_INTERVAL_MS = 500
export const POLL_TIMEOUT_MS = 15_000

export function countersOf(detail: VmDetail): Counters {
  return {
    guest_entry_count: detail.guest_entry_count ?? 0,
    guest_park_count: detail.guest_park_count ?? 0,
  }
}

/** 该操作此刻是否已到达真实终态。delete 没有可断言的状态，由 404 判定。 */
export function isSettled(op: LifecycleOp, before: Counters, after: VmDetail): boolean {
  switch (op) {
    case 'start':
    case 'resume':
      return (
        after.status === 'running' &&
        (after.guest_entry_count ?? 0) > before.guest_entry_count
      )
    case 'pause':
      return (
        after.status === 'paused' && (after.guest_park_count ?? 0) > before.guest_park_count
      )
    case 'stop':
      return after.status === 'stopped'
    case 'create':
      return after.status === 'ready'
    case 'delete':
      return false
  }
}

/** 等待中的提示语，超时后随观测值一起展示。 */
export function settleHint(op: LifecycleOp): string {
  switch (op) {
    case 'start':
    case 'resume':
      return '等待 vCPU 真正进入 guest（guest_entry_count 增长）'
    case 'pause':
      return '等待 vCPU 真正 park（guest_park_count 增长）'
    case 'stop':
      return '等待 vCPU 退出（status 变 stopped）'
    case 'create':
      return '等待注册完成（status 变 ready）'
    case 'delete':
      return '等待详情变 404'
  }
}

export interface SettleSuccess {
  ok: true
  detail?: VmDetail
}

export interface SettleTimeout {
  ok: false
  message: string
  /** 超时时最后一次观测到的详情；一次都没取到则为 undefined。 */
  detail?: VmDetail
}

export type SettleResult = SettleSuccess | SettleTimeout

export interface SettleDeps {
  now: () => number
  sleep: (ms: number) => Promise<void>
  intervalMs: number
  timeoutMs: number
}

const defaultDeps: SettleDeps = {
  now: () => Date.now(),
  sleep: (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
  intervalMs: POLL_INTERVAL_MS,
  timeoutMs: POLL_TIMEOUT_MS,
}

/** 超时文案：如实带上已观测到的状态与计数，方便定位卡在哪一步。 */
export function timeoutMessage(
  op: LifecycleOp,
  last: VmDetail | undefined,
  lastError: unknown,
): string {
  const observed =
    last === undefined
      ? '未取到详情'
      : `最后观测 status=${last.status} entry=${last.guest_entry_count ?? '-'} park=${last.guest_park_count ?? '-'}`
  const failure = lastError === undefined ? '' : `（最近一次请求错误：${String(lastError)}）`
  return `${settleHint(op)}超时：${observed}${failure}`
}

/**
 * 轮询详情直到终态断言成立。
 *
 * 采样基线由调用方在动作**之前**取好并传入（`before`）。
 * 轮询期间的 404 只对 delete 视为成功；其它错误继续轮询到超时，
 * 因为一次瞬时失败不代表操作没有生效。
 */
export async function settleToTerminalState(
  op: LifecycleOp,
  before: Counters,
  fetchDetail: () => Promise<VmDetail>,
  deps: SettleDeps = defaultDeps,
): Promise<SettleResult> {
  const deadline = deps.now() + deps.timeoutMs
  let last: VmDetail | undefined
  let lastError: unknown

  for (;;) {
    try {
      const detail = await fetchDetail()
      last = detail
      lastError = undefined
      if (isSettled(op, before, detail)) {
        return { ok: true, detail }
      }
    } catch (error) {
      lastError = error
      if (op === 'delete' && error instanceof ApiError && error.status === 404) {
        return { ok: true }
      }
    }

    if (deps.now() >= deadline) {
      return { ok: false, message: timeoutMessage(op, last, lastError), detail: last }
    }
    await deps.sleep(deps.intervalMs)
  }
}
