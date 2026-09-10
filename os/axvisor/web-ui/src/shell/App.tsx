//! 应用壳：token 门 → 布局 → 标签栏 → 导航（manifest 驱动）。
//!
//! 不变量 5：本目录下没有一处 import panels/*，也没有任何 kind 字面量。
//! 面板怎么渲染由注入进来的 registry 决定（types.ts 里的 PanelRegistry 契约）。

import { useCallback, useEffect, useRef, useState } from 'react'
import { useApiClient } from '@/api/client'
import { useVmFeed } from '@/api/feed'
import { describeError, type Manifest, type PanelRegistry } from '@/api/types'
import { Button } from '@/components/ui/button'
import { Nav } from './Nav'
import { Tabs } from './Tabs'
import { TokenGate } from './TokenGate'

export interface TabState {
  /** 标签实例 id：同 kind 可以开多个实例（终端阶段的独占语义靠它演示） */
  id: string
  kind: string
}

export default function App({ registry }: { registry: PanelRegistry }) {
  const [token, setToken] = useState<string | null>(null)

  if (token === null) return <TokenGate onSubmit={setToken} />
  return <Shell registry={registry} token={token} />
}

function Shell({ registry, token }: { registry: PanelRegistry; token: string }) {
  const api = useApiClient(token)
  const [manifest, setManifest] = useState<Manifest | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [reloadKey, setReloadKey] = useState(0)
  const [tabs, setTabs] = useState<TabState[]>([])
  const [activeId, setActiveId] = useState<string | null>(null)
  const bootstrappedRef = useRef(false)

  useEffect(() => {
    const ac = new AbortController()
    api
      .get<Manifest>('/api/', ac.signal)
      .then((m) => {
        setManifest(m)
        setError(null)
        // 默认打开第一个面板，省掉一次点击
        if (!bootstrappedRef.current) {
          bootstrappedRef.current = true
          const first = m.resources[0]
          if (first) {
            const tab: TabState = { id: tabId(0), kind: first.kind }
            setTabs([tab])
            setActiveId(tab.id)
          }
        }
      })
      .catch((e: unknown) => {
        if (ac.signal.aborted) return
        setError(describeError(e))
      })
    return () => ac.abort()
  }, [api, reloadKey])

  // 壳级资源快照：跟随 manifest 给 vms 资源的 href，不硬编码端点。
  const vmsHref = manifest?.resources.find((r) => r.kind === 'vms')?.href ?? null
  const feed = useVmFeed(api, vmsHref)

  // 关掉当前标签后，退回到还开着的最近一个
  useEffect(() => {
    if (activeId === null && tabs.length > 0) {
      setActiveId(tabs[tabs.length - 1].id)
    }
  }, [activeId, tabs])

  const openPanel = useCallback(
    (kind: string) => {
      // 导航语义：这种 kind 已有实例就聚焦它，没有才新建
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
      // 「+」语义：无条件新开一个实例（每标签一个独立面板实例）
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
          {/* 重扫 manifest：后端插拔了能力，点一下导航就跟着变 */}
          <Button size="sm" variant="ghost" onClick={() => setReloadKey((k) => k + 1)}>
            刷新能力清单
          </Button>
        </div>
      </header>

      {error && (
        <div className="flex items-center justify-between gap-4 border-b bg-destructive/10 px-4 py-2 text-sm">
          {/* 错误消息同时给出 HTTP 状态与后端错误两个维度（不变量 11） */}
          <span className="font-mono text-destructive">{error}</span>
          <Button size="sm" variant="outline" onClick={() => setReloadKey((k) => k + 1)}>
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
          onOpenVm={() => openPanel('vms')}
        />
        <Tabs
          resources={manifest?.resources ?? []}
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
