//! 壳级资源快照。
//!
//! 本期后端没有 `/ws/events` 事件流，先按固定周期轮询资源根，并暴露
//! `{vms, live, refresh}`。接口刻意对齐将来的事件订阅实现，届时只换本文件
//! 的内部实现（轮询 → 订阅），壳与面板零改动。

import { useCallback, useEffect, useRef, useState } from 'react'
import type { ApiClient } from './client'
import type { VmInfo } from './types'

const POLL_INTERVAL_MS = 2000

export interface VmFeed {
  vms: VmInfo[]
  /** 最近一次轮询是否成功。false 时壳显示「轮询中」，不代表数据已清空。 */
  live: boolean
  /** 变更操作收口后调用，立刻重取一次快照。 */
  refresh: () => void
}

export function useVmFeed(api: ApiClient, href: string | null): VmFeed {
  const [vms, setVms] = useState<VmInfo[]>([])
  const [live, setLive] = useState(false)
  const [generation, setGeneration] = useState(0)
  const inFlight = useRef(false)

  const refresh = useCallback(() => setGeneration((g) => g + 1), [])

  useEffect(() => {
    if (href === null) return
    let stopped = false

    const tick = async () => {
      // 上一请求未返回则跳过本 tick，不排队堆积
      if (inFlight.current) return
      inFlight.current = true
      try {
        const list = await api.get<VmInfo[]>(href)
        if (!stopped) {
          setVms(list)
          setLive(true)
        }
      } catch {
        if (!stopped) setLive(false)
      } finally {
        inFlight.current = false
      }
    }

    void tick()
    const timer = window.setInterval(() => void tick(), POLL_INTERVAL_MS)
    return () => {
      stopped = true
      window.clearInterval(timer)
    }
  }, [api, href, generation])

  return { vms, live, refresh }
}
