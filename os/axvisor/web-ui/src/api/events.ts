//! Live VM registry feed (the `vms` panel's `events` link, `browser-console` builds).
//!
//! The frames only say *that* something changed; `GET /api/vms` stays
//! authoritative. The feed therefore keeps a list that is replaced wholesale by
//! the `snapshot` frame at connect and then patched by `created` / `removed` /
//! `status`, which is enough for a navigation list that must not poll.
//!
//! A dropped connection is retried with a capped backoff: the control plane may
//! simply be busy creating a VM when the browser reconnects.

import { useEffect, useState } from 'react'
import type { VmStatus, VmSummary } from './types'
import { wsUrl } from './ws'

/** One frame of the event socket. */
type VmFrame =
  | { type: 'snapshot'; vms: { id: number; name: string; status: VmStatus }[] }
  | { type: 'created' | 'removed' | 'status'; id: number; name: string; status: VmStatus }

export interface VmFeed {
  vms: VmSummary[]
  /** Whether the socket is currently connected (surfaced by the navigation). */
  live: boolean
}

export function useVmFeed(url: string | null): VmFeed {
  const [vms, setVms] = useState<VmSummary[]>([])
  const [live, setLive] = useState(false)

  useEffect(() => {
    // A build without the feed declares no such link, and the caller passes
    // `null`: there is nothing to connect to, and no error to report either.
    if (url === null) {
      setLive(false)
      return
    }
    let socket: WebSocket | null = null
    let closedByUs = false
    let retries = 0
    let timer: number | undefined

    const connect = () => {
      if (closedByUs) return
      socket = new WebSocket(wsUrl(url))
      socket.onopen = () => {
        retries = 0
        setLive(true)
      }
      socket.onmessage = (event: MessageEvent) => {
        if (typeof event.data !== 'string') return
        let frame: VmFrame
        try {
          frame = JSON.parse(event.data) as VmFrame
        } catch {
          return // Not a JSON frame: the protocol has none, so ignore it.
        }
        setVms((previous) => applyFrame(previous, frame))
      }
      socket.onclose = () => {
        if (closedByUs) return
        setLive(false)
        const delay = Math.min(8000, 500 * 2 ** retries++)
        timer = window.setTimeout(connect, delay)
      }
    }
    connect()

    return () => {
      closedByUs = true
      if (timer !== undefined) window.clearTimeout(timer)
      socket?.close()
    }
  }, [url])

  return { vms, live }
}

function applyFrame(previous: VmSummary[], frame: VmFrame): VmSummary[] {
  if (frame.type === 'snapshot') {
    return frame.vms.map((vm) => ({ ...vm, cpu_num: 0, memory_mb: 0 }))
  }
  if (frame.type === 'removed') {
    return previous.filter((vm) => vm.id !== frame.id)
  }
  const known = previous.some((vm) => vm.id === frame.id)
  if (frame.type === 'created' && known) {
    return previous.map((vm) =>
      vm.id === frame.id ? { ...vm, name: frame.name, status: frame.status } : vm,
    )
  }
  if (!known) {
    return [...previous, { id: frame.id, name: frame.name, status: frame.status, cpu_num: 0, memory_mb: 0 }]
  }
  return previous.map((vm) =>
    vm.id === frame.id ? { ...vm, name: frame.name, status: frame.status } : vm,
  )
}
