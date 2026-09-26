//! Application shell: manifest → navigation → tabs → panels.
//!
//! Nothing under `shell/` imports `panels/` and no panel kind appears here except
//! the one the registry names as its terminal, which is a registry fact: how a
//! panel renders is decided by the injected registry, and which panels exist is
//! decided by the backend manifest. That is what makes the navigation follow the
//! build — a `http-axum`-only hypervisor advertises VM management, a build with
//! `browser-console` adds the terminals — without a frontend change.
//!
//! The shell reads the manifest once and turns it into [`Capabilities`]: the
//! panels below are handed accessors, never paths. The feed URL is looked up the
//! same way, so a build without the event link simply has no live feed.

import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { useApiClient } from '@/api/client'
import { useVmFeed } from '@/api/events'
import { describeError, type Manifest, type PanelRegistry } from '@/api/types'
import { Capabilities } from '@/capability/accessor'
import { loadManifest } from '@/capability/manifest'
import { Button } from '@/components/ui/button'
import { FilesService } from '@/domain/files'
import { Nav } from './Nav'
import { Tabs } from './Tabs'

export interface TabState {
  /** Tab instance id: one kind may have several instances at the same time. */
  id: string
  kind: string
}

export default function App({ registry }: { registry: PanelRegistry }) {
  const api = useApiClient()
  const [manifest, setManifest] = useState<Manifest | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [reloadKey, setReloadKey] = useState(0)
  const [tabs, setTabs] = useState<TabState[]>([])
  const [activeId, setActiveId] = useState<string | null>(null)
  const [focusVm, setFocusVm] = useState<number | null>(null)
  const bootstrappedRef = useRef(false)
  const capabilities = useMemo(() => new Capabilities(manifest?.panels ?? []), [manifest])
  // The one cross-panel lookup in the shell: the registry list on the left follows
  // the `vms` panel's event link. A build that does not declare it keeps a static
  // list rather than retrying a route that does not exist.
  const feedUrl = capabilities.maybeUrl('vms', 'events')
  const { vms, live } = useVmFeed(feedUrl)
  // The transfer store, built once for the whole shell: the `files` panel drives
  // it and a creation form uses it to put a file where a config names one. It is
  // built here rather than in a panel because panels do not import each other —
  // the composition root hands the same store to both. A build without the
  // `files` panel has no transfers and therefore no store.
  const files = useMemo(
    () => (capabilities.panel('files') ? new FilesService(api, capabilities.bind('files')) : null),
    [api, capabilities],
  )

  useEffect(() => {
    const controller = new AbortController()
    loadManifest(api, controller.signal)
      .then((fetched) => {
        setManifest(fetched)
        setError(null)
        // Open the first declared panel so an operator lands on something useful.
        if (!bootstrappedRef.current) {
          bootstrappedRef.current = true
          const first = fetched.panels[0]
          if (first) {
            const tab = { id: tabId(0), kind: first.kind }
            setTabs([tab])
            setActiveId(tab.id)
          }
        }
      })
      .catch((e: unknown) => {
        if (controller.signal.aborted) return
        setError(describeError(e))
      })
    return () => controller.abort()
  }, [api, reloadKey])

  // After the active tab is closed, fall back to the most recently opened one.
  useEffect(() => {
    if (activeId === null && tabs.length > 0) setActiveId(tabs[tabs.length - 1].id)
  }, [activeId, tabs])

  const openPanel = useCallback(
    (kind: string) => {
      const existing = tabs.find((tab) => tab.kind === kind)
      if (existing) {
        setActiveId(existing.id)
        return
      }
      const tab = { id: tabId(tabs.length), kind }
      setTabs((previous) => [...previous, tab])
      setActiveId(tab.id)
    },
    [tabs],
  )

  const newTab = useCallback(
    (kind: string) => {
      const tab = { id: tabId(tabs.length), kind }
      setTabs((previous) => [...previous, tab])
      setActiveId(tab.id)
    },
    [tabs],
  )

  const closeTab = useCallback((id: string) => {
    setTabs((previous) => previous.filter((tab) => tab.id !== id))
    setActiveId((current) => (current === id ? null : current))
  }, [])

  // Clicking a resource opens the terminal panel for it, if the build has one.
  const openVm = useCallback(
    (id: number) => {
      const terminal = registry.terminalKind
      if (terminal && manifest?.panels.some((panel) => panel.kind === terminal)) openPanel(terminal)
      setFocusVm(id)
    },
    [manifest, openPanel, registry],
  )

  const panels = manifest?.panels ?? []

  return (
    <div className="flex h-screen flex-col">
      <header className="flex items-center justify-between border-b px-4 py-2">
        <span className="text-sm font-semibold">AxVisor 管理台</span>
        <div className="flex items-center gap-3">
          <span className="text-xs text-muted-foreground">
            manifest proto {manifest?.proto ?? '-'} · {panels.length} 个面板
          </span>
          {/* Re-reading the manifest is how a capability change becomes visible
              without a page reload: the navigation follows the declaration. */}
          <Button size="sm" variant="ghost" onClick={() => setReloadKey((key) => key + 1)}>
            刷新能力清单
          </Button>
        </div>
      </header>

      {error && (
        <div className="flex items-center justify-between gap-4 border-b bg-destructive/10 px-4 py-2 text-sm">
          <span className="font-mono text-destructive">{error}</span>
          <Button size="sm" variant="outline" onClick={() => setReloadKey((key) => key + 1)}>
            重试
          </Button>
        </div>
      )}

      <div className="flex min-h-0 flex-1">
        <Nav
          panels={panels}
          resources={vms}
          live={live}
          activeKind={tabs.find((tab) => tab.id === activeId)?.kind ?? null}
          onOpen={openPanel}
          onOpenVm={openVm}
        />
        <Tabs
          panels={panels}
          tabs={tabs}
          activeId={activeId}
          registry={registry}
          api={api}
          capabilities={capabilities}
          files={files}
          resources={vms}
          focusVm={focusVm}
          onActivate={setActiveId}
          onClose={closeTab}
          onNew={newTab}
        />
      </div>
    </div>
  )
}

function tabId(index: number): string {
  return `tab-${index}-${Math.random().toString(36).slice(2, 7)}`
}
