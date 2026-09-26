//! Navigation: every entry comes from the manifest.
//!
//! No panel kind is hardcoded here — a node the backend adds shows up as an entry
//! — and the resource list below it is the live registry snapshot the shell
//! receives over `/ws/events`, so it changes without a poll.

import { Badge } from '@/components/ui/badge'
import { describeStatus, type PanelMeta, type VmSummary } from '@/api/types'
import { STATUS_TONE } from '@/lib/status'
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
    <nav className="flex w-60 shrink-0 flex-col overflow-y-auto border-r bg-muted/30">
      <div className="px-4 py-3 text-xs font-medium uppercase tracking-wide text-muted-foreground">
        能力 · 来自 manifest
      </div>
      <ul className="flex flex-col gap-1 px-2">
        {panels.map((panel) => (
          <li key={panel.kind}>
            <button
              type="button"
              onClick={() => onOpen(panel.kind)}
              className={cn(
                'flex w-full flex-col items-start gap-1 rounded-md px-3 py-2 text-left transition-colors',
                panel.kind === activeKind
                  ? 'bg-primary text-primary-foreground'
                  : 'hover:bg-accent hover:text-accent-foreground',
              )}
            >
              <span className="text-sm font-medium">{panel.title}</span>
              <span className="flex flex-wrap gap-1">
                {panel.verbs.map((verb) => (
                  <Badge key={verb} variant="outline" className="text-[10px]">
                    {verb}
                  </Badge>
                ))}
              </span>
            </button>
          </li>
        ))}
      </ul>
      {panels.length === 0 && (
        <p className="px-4 py-2 text-sm text-muted-foreground">manifest 未暴露任何面板</p>
      )}

      <div className="mt-2 flex items-center justify-between px-4 py-2 text-xs font-medium uppercase tracking-wide text-muted-foreground">
        <span>客户机 · 实时</span>
        <span
          className={cn(
            'rounded-full px-1.5 py-0.5 text-[10px]',
            live ? 'bg-emerald-100 text-emerald-700' : 'bg-amber-100 text-amber-700',
          )}
          title="事件通道连接状态"
        >
          {live ? '已连接' : '未连接'}
        </span>
      </div>
      <ul className="flex flex-col gap-1 px-2 pb-4">
        {resources.map((vm) => (
          <li key={vm.id}>
            <button
              type="button"
              onClick={() => onOpenVm(vm.id)}
              className="flex w-full items-center justify-between rounded-md px-3 py-2 text-left text-sm hover:bg-accent hover:text-accent-foreground"
            >
              <span className="font-mono">VM #{vm.id}</span>
              <span
                className={cn('rounded-full border px-2 py-0.5 text-[10px]', STATUS_TONE[vm.status] ?? '')}
              >
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
