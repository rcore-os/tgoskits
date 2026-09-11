//! Lifecycle settling: **HTTP 200 only means the request was accepted, not that the
//! terminal state was reached**.
//!
//! After every mutating operation this polls the VM detail until the assertion holds
//! or it times out. The assertion discipline matches `http_probe.py` in
//! `test-suit/axvisor/normal/qemu-http-control-plane`: start/resume require
//! `guest_entry_count` to strictly increase (a vCPU really entered the guest), pause
//! requires `guest_park_count` to strictly increase (a vCPU really parked, not just a
//! status flip), stop requires `stopped`, create requires `ready`, and delete requires
//! the detail to turn 404.
//!
//! Everything here is a pure function with injected dependencies, so vitest can drive
//! it deterministically.

import { ApiError, type VmDetail } from '@/api/types'

export type LifecycleOp = 'create' | 'start' | 'pause' | 'resume' | 'stop' | 'delete'

/** Counter baseline captured before the operation; only operations proven by "strictly increased" use it. */
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

/** Whether this operation has reached its real terminal state. delete has no assertable status; a 404 decides it. */
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

/** Hint shown while waiting; on timeout it is displayed together with the observed values. */
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
  /** Last detail observed before the timeout; undefined if none was ever fetched. */
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

/** Timeout text: reports the observed status and counters verbatim, so the stuck step is easy to locate. */
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
 * Polls the detail until the terminal-state assertion holds.
 *
 * The caller samples the baseline **before** the action and passes it as `before`.
 * A 404 during polling counts as success only for delete; other errors keep polling
 * until the timeout, because one transient failure does not mean the operation had
 * no effect.
 *
 * Every detail request receives an AbortSignal that fires once the deadline
 * passes, so a request that never settles cannot park the loop forever and
 * swallow the timeout.
 */
export async function settleToTerminalState(
  op: LifecycleOp,
  before: Counters,
  fetchDetail: (signal?: AbortSignal) => Promise<VmDetail>,
  deps: SettleDeps = defaultDeps,
): Promise<SettleResult> {
  const deadline = deps.now() + deps.timeoutMs
  let last: VmDetail | undefined
  let lastError: unknown

  for (;;) {
    const remaining = deadline - deps.now()
    if (remaining <= 0) {
      return { ok: false, message: timeoutMessage(op, last, lastError), detail: last }
    }

    const controller = new AbortController()
    const abortAt = setTimeout(() => controller.abort(), Math.max(remaining, 1))
    try {
      const detail = await fetchDetail(controller.signal)
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
    } finally {
      clearTimeout(abortAt)
    }

    if (deps.now() >= deadline) {
      return { ok: false, message: timeoutMessage(op, last, lastError), detail: last }
    }
    await deps.sleep(deps.intervalMs)
  }
}
