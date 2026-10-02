//! Navigation: every entry comes from the manifest.
//!
//! No panel kind is hardcoded here — a node the backend adds shows up as an
//! entry — and the resource list below it is the live registry snapshot the shell
//! receives over `/ws/events`, so it changes without a poll.
//!
//! The icon of an entry is chosen from what the panel *does*, not from what it
//! is called: the manifest declares its verbs, and a stream, a read-write
//! resource and a read-only one are three different things to reach for. That
//! keeps this file free of panel kinds while still not showing every entry with
//! the same glyph.

import { Boxes, Server, Terminal } from 'lucide-react'
import { describeStatus, type PanelMeta, type VmSummary } from '@/api/types'
import { STATUS_DOT } from '@/lib/status'
import { cn } from '@/lib/utils'

interface NavProps {
  panels: PanelMeta[]
  resources: VmSummary[]
  live: boolean
  activeKind: string | null
  onOpen: (kind: string) => void
  onOpenVm: (id: number) => void
}

export function Nav({ panels, resources, live, activeKind, onOpen, onOpenVm }: NavProps) {
  return (
    <nav className="flex w-60 shrink-0 flex-col overflow-y-auto border-r">
      <div className="px-4 pb-1.5 pt-3.5 eyebrow">能力 · 来自 manifest</div>
      <ul className="flex flex-col gap-0.5 px-2">
        {panels.map((panel) => {
          const Icon = panelIcon(panel)
          const active = panel.kind === activeKind
          return (
            <li key={panel.kind}>
              {/* The active entry is marked by a rule at the edge and one warm
                  accent, not by a filled block: a solid bar in a column of
                  entries is the loudest thing on the page and it is the one
                  thing that does not carry information. */}
              <button
                type="button"
                onClick={() => onOpen(panel.kind)}
                className={cn(
                  'group relative flex w-full items-center gap-2.5 rounded-md px-3 py-2 text-left transition-colors',
                  active ? 'bg-accent/60' : 'hover:bg-accent/40',
                )}
              >
                {active && (
                  <span className="absolute left-0 top-1/2 h-4 w-[2px] -translate-y-1/2 rounded-full bg-primary" />
                )}
                <Icon
                  className={cn(
                    'h-4 w-4 shrink-0',
                    active
                      ? 'text-primary'
                      : 'text-muted-foreground group-hover:text-foreground',
                  )}
                />
                <span className="flex min-w-0 flex-col leading-tight">
                  <span
                    className={cn(
                      'truncate text-sm font-medium',
                      active ? 'text-foreground' : 'text-foreground/85',
                    )}
                  >
                    {panel.title}
                  </span>
                  {/* The verbs stay — they are what the manifest declares about
                      what a panel may do — but as a caption: as badges they read
                      as debug output. */}
                  <span className="truncate font-mono text-[10px] text-muted-foreground">
                    {panel.verbs.join(' · ')}
                  </span>
                </span>
              </button>
            </li>
          )
        })}
      </ul>
      {panels.length === 0 && (
        <p className="px-4 py-2 text-sm text-muted-foreground">manifest 未暴露任何面板</p>
      )}

      <div className="mt-4 flex items-center justify-between px-4 pb-1.5 pt-1 eyebrow">
        <span>客户机 · 实时</span>
        <span
          className={cn(
            'flex items-center gap-1.5 rounded-full px-1.5 py-0.5 text-[10px] normal-case tracking-normal',
            live ? 'bg-signal/10 text-signal' : 'bg-warn/10 text-warn',
          )}
          title="事件通道连接状态"
        >
          <span
            className={cn(
              'h-1.5 w-1.5 rounded-full',
              live ? 'bg-signal' : 'bg-warn',
            )}
          />
          {live ? '已连接' : '未连接'}
        </span>
      </div>
      <ul className="flex flex-col gap-0.5 px-2 pb-4">
        {resources.map((vm) => (
          <li key={vm.id}>
            <button
              type="button"
              onClick={() => onOpenVm(vm.id)}
              className="flex w-full items-center gap-2 rounded-md px-3 py-2 text-left text-sm hover:bg-accent/40"
            >
              <span
                className={cn('h-1.5 w-1.5 shrink-0 rounded-full', STATUS_DOT[vm.status] ?? 'bg-muted-foreground')}
                title={describeStatus(vm.status)}
              />
              <span className="min-w-0 flex-1 truncate font-mono text-xs">VM[{vm.id}]</span>
              <span className="shrink-0 text-[10px] text-muted-foreground">
                {describeStatus(vm.status)}
              </span>
            </button>
          </li>
        ))}
        {resources.length === 0 && (
          <li className="px-4 py-1 text-sm text-muted-foreground">暂无客户机</li>
        )}
      </ul>
    </nav>
  )
}

/** Which glyph an entry gets, from the verbs the manifest declared for it. */
function panelIcon(panel: PanelMeta) {
  if (panel.verbs.includes('stream')) return Terminal
  if (panel.verbs.includes('write')) return Boxes
  return Server
}
