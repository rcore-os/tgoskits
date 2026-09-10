//! 应用壳：拉能力清单 → token 门 → 布局 → 标签栏 → 导航（manifest 驱动）。
//!
//! 不变量 5：本目录下没有一处 import panels/*，也没有任何 kind 字面量。
//! 面板怎么渲染由注入进来的 registry 决定（types.ts 里的 PanelRegistry 契约）。
//!
//! manifest 是**开放**端点（`GET /api/`），所以在登录前就取：token 门需要它声明
//! 的 auth 探测路径才能校验 token，而不是自己去猜一个端点。

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
  const [manifest, setManifest] = useState<Manifest | null>(null)
  const [manifestError, setManifestError] = useState<string | null>(null)
  const [reloadKey, setReloadKey] = useState(0)

  useEffect(() => {
    const ac = new AbortController()
    fetch('/api/', { signal: ac.signal })
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
      onResetToken={() => setToken(null)}
    />
  )
}

function Shell({
  registry,
  token,
  manifest,
  manifestError,
  onReloadManifest,
  onResetToken,
}: {
  registry: PanelRegistry
  token: string
  manifest: Manifest | null
  manifestError: string | null
  onReloadManifest: () => void
  onResetToken: () => void
}) {
  const api = useApiClient(token)
  const [tabs, setTabs] = useState<TabState[]>([])
  const [activeId, setActiveId] = useState<string | null>(null)
  const bootstrappedRef = useRef(false)

  // 默认打开第一个面板，省掉一次点击
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
          <Button size="sm" variant="ghost" onClick={onReloadManifest}>
            刷新能力清单
          </Button>
          {/* token 只存在内存态：写操作报 401 时唯一的补救就是重新输入，
             所以必须给一个入口，否则只能刷新页面。 */}
          <Button size="sm" variant="outline" onClick={onResetToken}>
            重设 token
          </Button>
        </div>
      </header>

      {manifestError && (
        <div className="flex items-center justify-between gap-4 border-b bg-destructive/10 px-4 py-2 text-sm">
          {/* 错误消息同时给出 HTTP 状态与后端错误两个维度（不变量 11） */}
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
          onOpenVm={() => openPanel('vms')}
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
