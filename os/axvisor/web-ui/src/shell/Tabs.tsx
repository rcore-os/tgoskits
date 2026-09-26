//! Tab strip: one panel instance per tab.
//!
//! Every opened tab stays mounted (inactive ones are hidden) so panels that hold
//! an exclusive resource — the console lanes are exclusive on the server — keep
//! their session when the operator switches away. The `+` menu opens extra
//! instances of any declared panel, which the shell supports without knowing what
//! the panels are.

import { Suspense, useState } from 'react'
import type { ApiClient } from '@/api/client'
import type { VmSummary } from '@/api/types'
import type { FilesCapability, PanelMeta, PanelRegistry } from '@/api/types'
import type { Capabilities } from '@/capability/accessor'
import { cn } from '@/lib/utils'
import { PanelErrorBoundary } from './PanelErrorBoundary'
import type { TabState } from './App'

interface TabsProps {
  panels: PanelMeta[]
  tabs: TabState[]
  activeId: string | null
  registry: PanelRegistry
  api: ApiClient
  /** Declared capabilities, bound per panel so no panel can name another's paths. */
  capabilities: Capabilities
  /**
   * The one transfer service, built by the shell: a panel never names another
   * resource's operations, so the shared store is injected rather than imported.
   */
  files: FilesCapability | null
  resources: VmSummary[]
  focusVm: number | null
  onActivate: (id: string) => void
  onClose: (id: string) => void
  onNew: (kind: string) => void
}

export function Tabs(props: TabsProps) {
  const {
    panels,
    tabs,
    activeId,
    registry,
    api,
    capabilities,
    files,
    resources,
    focusVm,
    onActivate,
    onClose,
    onNew,
  } = props
  const [pickerOpen, setPickerOpen] = useState(false)

  // Number the instances of a kind so two terminals are told apart.
  const totals = new Map<string, number>()
  for (const tab of tabs) totals.set(tab.kind, (totals.get(tab.kind) ?? 0) + 1)
  const seen = new Map<string, number>()

  return (
    <div className="flex min-w-0 flex-1 flex-col">
      <div className="flex items-center gap-1 border-b px-2 py-1">
        {tabs.map((tab) => {
          const meta = panels.find((panel) => panel.kind === tab.kind)
          if (!meta) return null
          const index = (seen.get(tab.kind) ?? 0) + 1
          seen.set(tab.kind, index)
          const multiple = (totals.get(tab.kind) ?? 0) > 1
          return (
            <div
              key={tab.id}
              className={cn(
                'flex items-center gap-2 rounded-md px-3 py-1.5 text-sm',
                tab.id === activeId ? 'bg-secondary' : 'text-muted-foreground hover:bg-accent',
              )}
            >
              <button type="button" onClick={() => onActivate(tab.id)}>
                {meta.title}
                {multiple && ` ·${index}`}
              </button>
              <button
                type="button"
                aria-label={`关闭 ${meta.title}`}
                className="text-muted-foreground hover:text-foreground"
                onClick={() => onClose(tab.id)}
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
            onClick={() => setPickerOpen((open) => !open)}
          >
            +
          </button>
          {pickerOpen && (
            <div className="absolute right-0 top-full z-10 mt-1 w-60 rounded-md border bg-popover p-2 shadow-md">
              <p className="px-2 pb-1 text-xs text-muted-foreground">
                新开一个面板实例（每标签独立）
              </p>
              {panels.map((panel) => (
                <button
                  key={panel.kind}
                  type="button"
                  className="flex w-full items-center justify-between rounded-md px-2 py-1.5 text-sm hover:bg-accent"
                  onClick={() => {
                    setPickerOpen(false)
                    onNew(panel.kind)
                  }}
                >
                  {panel.title}
                  <span className="font-mono text-xs text-muted-foreground">{panel.kind}</span>
                </button>
              ))}
              {panels.length === 0 && (
                <p className="px-2 py-1 text-xs text-muted-foreground">manifest 未暴露任何面板</p>
              )}
            </div>
          )}
        </div>
      </div>

      <div className="min-h-0 flex-1 overflow-auto p-4">
        {tabs.length === 0 && (
          <p className="text-sm text-muted-foreground">从左侧导航打开一个面板。</p>
        )}
        {tabs.map((tab) => {
          const meta = panels.find((panel) => panel.kind === tab.kind)
          if (!meta) return null
          const Panel = registry.resolve(tab.kind)
          // One stable accessor per kind: a panel may keep `link` in an effect's
          // dependency list without re-running on every render of this strip.
          const link = capabilities.bind(tab.kind)
          return (
            <div key={tab.id} className={cn('h-full', tab.id === activeId ? 'block' : 'hidden')}>
              {/* One boundary per tab: a panel that throws on one VM must not
                  take the shell, the navigation or the other tabs down. */}
              <PanelErrorBoundary title={meta.title}>
                <Suspense fallback={<p className="text-sm text-muted-foreground">加载面板…</p>}>
                  <Panel
                    meta={meta}
                    api={api}
                    link={link}
                    files={files}
                    resources={resources}
                    focusVm={focusVm}
                  />
                </Suspense>
              </PanelErrorBoundary>
            </div>
          )
        })}
      </div>
    </div>
  )
}
