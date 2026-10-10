//! Application shell: manifest → navigation → tabs → panels.
//!
//! Nothing under `shell/` imports `panels/` and no panel kind appears here except
//! the one the registry names as its terminal, which is a registry fact: how a
//! panel renders is decided by the injected registry, and which panels exist is
//! decided by the backend manifest. That is what makes the navigation follow the
//! build — a `web` advertises the available management and console panels without a frontend change.
//!
//! The shell reads the manifest once and turns it into [`Capabilities`]: the
//! panels below are handed accessors, never paths. The feed URL is looked up the
//! same way, so a build without the event link simply has no live feed.
//!
//! Three facts are read once here and injected into every panel, because a
//! panel cannot read another resource's operations: the manifest, the live
//! registry snapshot, and the host the hypervisor is running on.

import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { Search, Server } from 'lucide-react'
import { useApiClient } from '@/api/client'
import { useVmFeed } from '@/api/events'
import { describeError, type Manifest, type PanelRegistry, type VmSummary } from '@/api/types'
import { Capabilities } from '@/capability/accessor'
import { loadManifest } from '@/capability/manifest'
import { Button } from '@/components/ui/button'
import { FilesService } from '@/domain/files'
import { formatUptime } from '@/lib/format'
import { applyTheme, readTheme } from '@/lib/theme'
import { cn } from '@/lib/utils'
import { CommandPalette } from './CommandPalette'
import { Nav } from './Nav'
import { Tabs } from './Tabs'
import { ThemeToggle } from './ThemeToggle'

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
  const [paletteOpen, setPaletteOpen] = useState(false)
  const bootstrappedRef = useRef(false)
  const capabilities = useMemo(() => new Capabilities(manifest?.panels ?? []), [manifest])
  // The one cross-panel lookup in the shell: the registry list on the left follows
  // the `vms` panel's event link. A build that does not declare it keeps a static
  // list rather than retrying a route that does not exist.
  const feedUrl = capabilities.maybeUrl('vms', 'events')
  const registryUrl = capabilities.maybeUrl('vms', 'list')
  const refreshRegistry = useCallback(async (): Promise<VmSummary[]> => {
    if (registryUrl === null) return []
    return api.get<VmSummary[]>(registryUrl)
  }, [api, registryUrl])
  const { vms, live } = useVmFeed(
    feedUrl,
    registryUrl === null ? undefined : refreshRegistry,
  )
  // The host facts the header names and every panel is handed. They come from
  // the descriptor rather than from the host panel's own route, because the
  // shell may not name a panel kind: the descriptor publishes the same object.
  const host = manifest?.host ?? null
  // The transfer store, built once for the whole shell: the `files` panel drives
  // it and a creation form uses it to put a file where a config names one. It is
  // built here rather than in a panel because panels do not import each other —
  // the composition root hands the same store to both. A build without the
  // `files` panel has no transfers and therefore no store.
  //
  // Held in a ref rather than derived from `capabilities`, because re-reading the
  // manifest replaces that object. Building a new store on every refresh would
  // silently drop every in-flight transfer: the panels would go back to an empty
  // list with no error anywhere. The links a store was built with stay valid —
  // the descriptor is fixed for a given build — so the refresh that this does
  // not react to is the one thing a store has to outlive.
  const filesRef = useRef<FilesService | null>(null)
  if (filesRef.current === null && capabilities.panel('files')) {
    filesRef.current = new FilesService(api, capabilities.bind('files'))
  }
  const files = filesRef.current

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

  // The scheme is applied once, from what the last visit chose: every colour is
  // a variable with a value per scheme, so no component below knows which one
  // is on and nothing re-mounts when it changes.
  useEffect(() => {
    applyTheme(readTheme())
  }, [])

  // `⌘K`/`Ctrl+K` opens the palette, which is the one shortcut worth having on
  // a console: the alternative is a mouse trip to the navigation for every
  // switch between a guest list and a terminal.
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === 'k') {
        event.preventDefault()
        setPaletteOpen((open) => !open)
      }
    }
    window.addEventListener('keydown', onKeyDown)
    return () => window.removeEventListener('keydown', onKeyDown)
  }, [])

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
  const hostLine = host
    ? [host.version, host.arch, `${host.phys_cpu_count} pCPU`].filter((part) => part.length > 0).join(' · ')
    : null

  return (
    <div className="flex h-screen flex-col">
      <header className="flex items-center justify-between gap-4 border-b px-4 py-2.5">
        <div className="flex min-w-0 items-center gap-2.5">
          <span className="flex h-8 w-8 shrink-0 items-center justify-center rounded-md border border-primary/25 bg-primary/10 text-primary">
            <Server className="h-4 w-4" />
          </span>
          <span className="flex min-w-0 flex-col leading-tight">
            <span className="truncate text-sm font-semibold tracking-tight">AxVisor 管理台</span>
            <span className="truncate font-mono text-[11px] text-muted-foreground">
              {hostLine ?? `manifest proto ${manifest?.proto ?? '-'} · ${panels.length} 个面板`}
            </span>
          </span>
        </div>
        <div className="flex items-center gap-2">
          <button
            type="button"
            onClick={() => setPaletteOpen(true)}
            className="hidden items-center gap-2 rounded-md border px-2.5 py-1 text-xs text-muted-foreground transition-colors hover:bg-accent hover:text-accent-foreground md:flex"
          >
            <Search className="h-3.5 w-3.5" />
            搜索面板或客户机
            <kbd className="rounded border bg-muted px-1 font-mono text-[10px]">⌘K</kbd>
          </button>
          <span className="hidden font-mono text-[11px] text-muted-foreground lg:inline">
            proto {manifest?.proto ?? '-'} · {panels.length} 面板
          </span>
          <ThemeToggle />
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
          live={live}
          host={host}
          focusVm={focusVm}
          onActivate={setActiveId}
          onClose={closeTab}
          onNew={newTab}
        />
      </div>

      {/* A footer for the two facts that belong to the whole console and to no
          panel: whether the event feed is still open, and how long this
          hypervisor has been up. Both come from what the shell already reads. */}
      <footer className="flex shrink-0 items-center justify-between gap-4 border-t px-4 py-1 text-[11px] text-muted-foreground">
        <span className="flex min-w-0 items-center gap-1.5">
          <span
            className={cn('h-1.5 w-1.5 shrink-0 rounded-full', live ? 'bg-signal' : 'bg-warn')}
            title="事件通道连接状态"
          />
          {live ? '事件通道已连接' : '事件通道未连接'}
        </span>
        <span className="figure truncate">
          {host ? `已运行 ${formatUptime(host.uptime_secs)}` : `proto ${manifest?.proto ?? '-'}`}
        </span>
        <span className="figure hidden truncate md:inline">
          {[host?.platform, `${panels.length} 面板`]
            .filter((part): part is string => typeof part === 'string' && part.length > 0)
            .join(' · ')}
        </span>
      </footer>

      <CommandPalette
        open={paletteOpen}
        panels={panels}
        resources={vms}
        onOpen={openPanel}
        onOpenVm={openVm}
        onClose={() => setPaletteOpen(false)}
      />
    </div>
  )
}

function tabId(index: number): string {
  return `tab-${index}-${Math.random().toString(36).slice(2, 7)}`
}
