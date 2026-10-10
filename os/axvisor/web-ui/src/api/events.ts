//! Live VM registry feed (the `vms` panel's `events` link in `web` builds).
//!
//! The frames only say *that* something changed; `GET /api/vms` stays
//! authoritative. The feed therefore keeps a list that is replaced wholesale by
//! the `snapshot` frame at connect and then patched by `created` / `removed` /
//! `status`; the panel combines these eager updates with an authoritative HTTP
//! refresh so it can recover from dropped frames and retain full VM metadata.
//!
//! A dropped connection is retried with a capped backoff: the control plane may
//! simply be busy creating a VM when the browser reconnects. The shell also
//! refreshes the authoritative registry over HTTP, so a dropped event cannot
//! leave navigation or a command palette stale forever.

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

export function useVmFeed(
  url: string | null,
  refreshRegistry?: () => Promise<VmSummary[]>,
): VmFeed {
  const [vms, setVms] = useState<VmSummary[]>([])
  const [live, setLive] = useState(false)

  useEffect(() => {
    let socket: WebSocket | null = null
    let closedByUs = false
    let retries = 0
    let timer: number | undefined
    let registryTimer: number | undefined
    let refreshSequence = 0
    let eventSequence = 0

    const refresh = () => {
      if (refreshRegistry === undefined) return
      const sequence = ++refreshSequence
      const eventsSeen = eventSequence
      void refreshRegistry()
        .then((list) => {
          // A registry response can have been in flight while a WebSocket frame
          // announced a newer state. Do not let that older HTTP snapshot roll
          // the navigation back; the next interval will fetch it again.
          if (sequence === refreshSequence && eventsSeen === eventSequence) setVms(list)
        })
        .catch(() => {
          // The event snapshot, when available, remains useful while the
          // control plane is busy. The next interval retries the HTTP read.
        })
    }
    refresh()
    if (refreshRegistry !== undefined) {
      registryTimer = window.setInterval(refresh, 2000)
    }

    // A build without the feed declares no such link, and the caller passes
    // `null`: keep the HTTP registry fallback when one exists, but do not open
    // a socket or report it as live.
    if (url === null) {
      setLive(false)
      if (refreshRegistry === undefined) setVms([])
      return () => {
        closedByUs = true
        if (registryTimer !== undefined) window.clearInterval(registryTimer)
      }
    }

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
        eventSequence += 1
        setVms((previous) => applyFrame(previous, frame))
      }
      socket.onclose = () => {
        if (closedByUs) return
        setLive(false)
        refresh()
        const delay = Math.min(8000, 500 * 2 ** retries++)
        timer = window.setTimeout(connect, delay)
      }
    }
    connect()

    return () => {
      closedByUs = true
      if (timer !== undefined) window.clearTimeout(timer)
      if (registryTimer !== undefined) window.clearInterval(registryTimer)
      socket?.close()
    }
  }, [refreshRegistry, url])

  return { vms, live }
}

function applyFrame(previous: VmSummary[], frame: VmFrame): VmSummary[] {
  if (frame.type === 'snapshot') {
    const known = new Map(previous.map((vm) => [vm.id, vm]))
    return frame.vms.map((vm) => {
      const current = known.get(vm.id)
      return current === undefined
        ? { ...vm, cpu_num: 0, memory_mb: 0 }
        : { ...current, name: vm.name, status: vm.status }
    })
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
