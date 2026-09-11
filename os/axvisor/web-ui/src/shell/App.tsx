//! Application shell: fetch the capability manifest -> token gate -> layout -> tab bar
//! -> navigation, all manifest driven.
//!
//! Invariant 5: nothing under this directory imports panels/*, and there is no kind
//! literal. How a panel renders is decided by the injected registry (the PanelRegistry
//! contract in types.ts).
//!
//! The manifest is an **open** endpoint (`GET /api/`), so it is fetched before sign-in:
//! the token gate needs the auth probe path it declares in order to verify the token,
//! rather than guessing an endpoint itself.

import { useCallback, useEffect, useRef, useState } from 'react'
import { useApiClient } from '@/api/client'
import { useVmFeed } from '@/api/feed'
import { describeError, type Manifest, type PanelRegistry } from '@/api/types'
import { Button } from '@/components/ui/button'
import { Nav } from './Nav'
import { Tabs } from './Tabs'
import { TokenGate } from './TokenGate'

export interface TabState {
  /** Tab instance id: one kind may have several instances (the terminal phase demos exclusive semantics through it). */
  id: string
  kind: string
}

// Root path used to fetch the manifest during bootstrap: it is not available yet, so
// this one hardcoded value is hoisted into a constant.
const MANIFEST_HREF = '/api/'
// Single source of truth for the resource kind: the shell and the panels both
// reference it instead of scattering 'vms' literals.
const VM_KIND = 'vms'

export default function App({ registry }: { registry: PanelRegistry }) {
  const [token, setToken] = useState<string | null>(null)
  const [manifest, setManifest] = useState<Manifest | null>(null)
  const [manifestError, setManifestError] = useState<string | null>(null)
  const [reloadKey, setReloadKey] = useState(0)

  useEffect(() => {
    const ac = new AbortController()
    fetch(MANIFEST_HREF, { signal: ac.signal })
      .then(async (res) => {
        if (!res.ok) throw new Error(`能力清单返回 HTTP ${res.status}`)
        return (await res.json()) as Manifest
      })
      .then((fetched) => {
        setManifest(fetched)
        setManifestError(null)
      })
      .catch((e: unknown) => {
        if (ac.signal.aborted) return
        setManifestError(describeError(e))
      })
    return () => ac.abort()
  }, [reloadKey])

  const reloadManifest = useCallback(() => setReloadKey((k) => k + 1), [])

  if (token === null) {
    return (
      <TokenGate
        auth={manifest?.auth ?? null}
        manifestError={manifestError}
        onRetryManifest={reloadManifest}
        onSubmit={setToken}
      />
    )
  }

  return (
    <Shell
      registry={registry}
      token={token}
      manifest={manifest}
      manifestError={manifestError}
      onReloadManifest={reloadManifest}
    />
  )
}

function Shell({
  registry,
  token,
  manifest,
  manifestError,
  onReloadManifest,
}: {
  registry: PanelRegistry
  token: string
  manifest: Manifest | null
  manifestError: string | null
  onReloadManifest: () => void
}) {
  const api = useApiClient(token, manifest?.auth?.scheme)
  const [tabs, setTabs] = useState<TabState[]>([])
  const [activeId, setActiveId] = useState<string | null>(null)
  const bootstrappedRef = useRef(false)

  // Open the first panel by default, saving one click.
  useEffect(() => {
    if (manifest === null || bootstrappedRef.current) return
    bootstrappedRef.current = true
    const first = manifest.resources[0]
    if (first) {
      const tab: TabState = { id: tabId(0), kind: first.kind }
      setTabs([tab])
      setActiveId(tab.id)
    }
  }, [manifest])

  // Shell-level resource snapshot: follows the href the manifest assigns to the vms
  // resource instead of hardcoding an endpoint.
  const vmsHref = manifest?.resources.find((r) => r.kind === VM_KIND)?.href ?? null
  const feed = useVmFeed(api, vmsHref)

  // After closing the active tab, fall back to the most recently opened one still present.
  useEffect(() => {
    if (activeId === null && tabs.length > 0) {
      setActiveId(tabs[tabs.length - 1].id)
    }
  }, [activeId, tabs])

  const openPanel = useCallback(
    (kind: string) => {
      // Navigation semantics: focus an existing instance of this kind, otherwise create one.
      const existing = tabs.find((t) => t.kind === kind)
      if (existing) {
        setActiveId(existing.id)
        return
      }
      const tab = { id: tabId(tabs.length), kind }
      setTabs((prev) => [...prev, tab])
      setActiveId(tab.id)
    },
    [tabs],
  )

  const newTab = useCallback(
    (kind: string) => {
      // "+" semantics: always open a new instance (one independent panel instance per tab).
      const tab = { id: tabId(tabs.length), kind }
      setTabs((prev) => [...prev, tab])
      setActiveId(tab.id)
    },
    [tabs],
  )

  const closeTab = useCallback((id: string) => {
    setTabs((prev) => prev.filter((t) => t.id !== id))
    setActiveId((cur) => (cur === id ? null : cur))
  }, [])

  return (
    <div className="flex h-screen flex-col">
      <header className="flex items-center justify-between border-b px-4 py-2">
        <span className="text-sm font-semibold">Axvisor</span>
        <div className="flex items-center gap-3">
          <span className="text-xs text-muted-foreground">
            manifest proto {manifest?.proto ?? '-'} · {manifest?.resources.length ?? 0} 个资源
          </span>
          {/* Rescan the manifest: when the backend adds or removes a capability, one click updates the navigation. */}
          <Button size="sm" variant="ghost" onClick={onReloadManifest}>
            刷新能力清单
          </Button>
        </div>
      </header>

      {manifestError && (
        <div className="flex items-center justify-between gap-4 border-b bg-destructive/10 px-4 py-2 text-sm">
          {/* The error message surfaces both the HTTP status and the backend error (invariant 11). */}
          <span className="font-mono text-destructive">{manifestError}</span>
          <Button size="sm" variant="outline" onClick={onReloadManifest}>
            重试
          </Button>
        </div>
      )}

      <div className="flex min-h-0 flex-1">
        <Nav
          resources={manifest?.resources ?? []}
          vms={feed.vms}
          live={feed.live}
          activeKind={activeKindOf(tabs, activeId)}
          onOpen={openPanel}
          onOpenVm={() => openPanel(VM_KIND)}
        />
        <Tabs
          resources={manifest?.resources ?? []}
          auth={manifest?.auth}
          tabs={tabs}
          activeId={activeId}
          registry={registry}
          api={api}
          token={token}
          vms={feed.vms}
          refresh={feed.refresh}
          onActivate={setActiveId}
          onClose={closeTab}
          onNew={newTab}
        />
      </div>
    </div>
  )
}

function tabId(n: number): string {
  return `tab-${n}-${Math.random().toString(36).slice(2, 7)}`
}

function activeKindOf(tabs: TabState[], activeId: string | null): string | null {
  return tabs.find((t) => t.id === activeId)?.kind ?? null
}
