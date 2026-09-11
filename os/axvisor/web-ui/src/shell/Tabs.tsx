//! Tabs: one panel instance per tab.
//!
//! Every opened tab stays mounted (inactive ones are hidden with `hidden`), so that a
//! future "exclusive subscription" panel such as the terminal really does hit the 409
//! when two tabs hold it at once.
//!
//! "+" opens a new instance: pick one of the manifest resources and unconditionally
//! open a new tab.

import { Suspense, useState } from 'react'
import type { ApiClient } from '@/api/client'
import type { AuthProbe, PanelRegistry, ResourceMeta, VmInfo } from '@/api/types'
import { cn } from '@/lib/utils'
import type { TabState } from './App'

interface TabsProps {
  resources: ResourceMeta[]
  tabs: TabState[]
  activeId: string | null
  registry: PanelRegistry
  api: ApiClient
  token: string
  /** Shell-level resource snapshot passed through to panels (used by resource panels, ignored by the rest). */
  vms: VmInfo[]
  /** Refreshes the shell-level snapshot as soon as a mutating operation settles. */
  refresh: () => void
  /** Auth scheme declared by the manifest, passed through to panels for danger confirmations. */
  auth?: AuthProbe
  onActivate: (id: string) => void
  onClose: (id: string) => void
  onNew: (kind: string) => void
}

export function Tabs(props: TabsProps) {
  const {
    resources,
    tabs,
    activeId,
    registry,
    api,
    token,
    vms,
    refresh,
    auth,
    onActivate,
    onClose,
    onNew,
  } = props
  const [pickerOpen, setPickerOpen] = useState(false)

  // Number tabs when one kind has several instances, so each session is identifiable.
  const counts = new Map<string, number>()
  for (const t of tabs) counts.set(t.kind, (counts.get(t.kind) ?? 0) + 1)
  const seen = new Map<string, number>()

  return (
    <div className="flex min-w-0 flex-1 flex-col">
      <div className="flex items-center gap-1 border-b px-2 py-1">
        {tabs.map((t) => {
          const meta = resources.find((r) => r.kind === t.kind)
          if (!meta) return null
          const index = (seen.get(t.kind) ?? 0) + 1
          seen.set(t.kind, index)
          const multi = (counts.get(t.kind) ?? 0) > 1
          return (
            <div
              key={t.id}
              className={cn(
                'flex items-center gap-2 rounded-md px-3 py-1.5 text-sm',
                t.id === activeId ? 'bg-secondary' : 'text-muted-foreground hover:bg-accent',
              )}
            >
              <button type="button" onClick={() => onActivate(t.id)}>
                {meta.title}
                {multi && ` ·${index}`}
              </button>
              <button
                type="button"
                aria-label={`关闭 ${meta.title}`}
                className="text-muted-foreground hover:text-foreground"
                onClick={() => onClose(t.id)}
              >
                ×
              </button>
            </div>
          )
        })}
        <div className="relative">
          <button
            type="button"
            aria-label="新开面板实例"
            title="新开面板实例"
            className="rounded-md px-2.5 py-1.5 text-muted-foreground hover:bg-accent hover:text-foreground"
            onClick={() => setPickerOpen((o) => !o)}
          >
            +
          </button>
          {pickerOpen && (
            <div className="absolute right-0 top-full z-10 mt-1 w-60 rounded-md border bg-popover p-2 shadow-md">
              <p className="px-2 pb-1 text-xs text-muted-foreground">
                新开一个面板实例（每标签独立）
              </p>
              {resources.map((r) => (
                <button
                  key={r.kind}
                  type="button"
                  className="flex w-full items-center justify-between rounded-md px-2 py-1.5 text-sm hover:bg-accent"
                  onClick={() => {
                    setPickerOpen(false)
                    onNew(r.kind)
                  }}
                >
                  {r.title}
                  <span className="font-mono text-xs text-muted-foreground">{r.kind}</span>
                </button>
              ))}
              {resources.length === 0 && (
                <p className="px-2 py-1 text-xs text-muted-foreground">manifest 未暴露任何资源</p>
              )}
            </div>
          )}
        </div>
      </div>

      <div className="min-h-0 flex-1 overflow-auto p-4">
        {tabs.length === 0 && (
          <p className="text-sm text-muted-foreground">从左侧导航打开一个面板。</p>
        )}
        {tabs.map((t) => {
          const meta = resources.find((r) => r.kind === t.kind)
          if (!meta) return null
          const Panel = registry.resolve(t.kind)
          return (
            <div key={t.id} className={cn('h-full', t.id === activeId ? 'block' : 'hidden')}>
              <Suspense fallback={<p className="text-sm text-muted-foreground">加载面板…</p>}>
                <Panel
                  meta={meta}
                  api={api}
                  token={token}
                  resources={vms}
                  refresh={refresh}
                  auth={auth}
                />
              </Suspense>
            </div>
          )
        })}
      </div>
    </div>
  )
}
