//! Shell-level resource snapshot.
//!
//! The backend has no `/ws/events` stream yet, so this polls the resource root on
//! a fixed interval and exposes `{vms, live, refresh}`. The interface is
//! deliberately shaped like the future event subscription: when that lands, only
//! the internals of this file change (polling -> subscription) and the shell and
//! panels stay untouched.

import { useCallback, useEffect, useRef, useState } from 'react'
import type { ApiClient } from './client'
import type { VmInfo } from './types'

const POLL_INTERVAL_MS = 2000

export interface VmFeed {
  vms: VmInfo[]
  /** Whether the most recent poll succeeded. When false the shell shows "polling"; it does not mean the data was cleared. */
  live: boolean
  /** Call once a mutating operation settles, to refetch the snapshot immediately. */
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
      // Skip this tick if the previous request has not returned; never queue up.
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
