//! Navigation items come 100% from the manifest (invariant 5): no kind is hardcoded
//! here, so one new manifest node means one more navigation entry.

import { Badge } from '@/components/ui/badge'
import { describeStatus, type ResourceMeta, type VmInfo } from '@/api/types'

interface NavProps {
  /** Resource families exposed by the manifest (capability area). */
  resources: ResourceMeta[]
  /** Shell-level resource snapshot (polled; see api/feed.ts). */
  vms: VmInfo[]
  live: boolean
  activeKind: string | null
  onOpen: (kind: string) => void
  onOpenVm: (id: number) => void
}

export function Nav({ resources, vms, live, activeKind, onOpen, onOpenVm }: NavProps) {
  return (
    <nav className="flex w-60 shrink-0 flex-col overflow-y-auto border-r bg-muted/30">
      <div className="px-4 py-3 text-xs font-medium uppercase tracking-wide text-muted-foreground">
        能力 · 来自 manifest
      </div>
      <ul className="flex flex-col gap-1 px-2">
        {resources.map((r) => (
          <li key={r.kind}>
            <button
              type="button"
              onClick={() => onOpen(r.kind)}
              className={
                'flex w-full flex-col items-start gap-1 rounded-md px-3 py-2 text-left transition-colors ' +
                (r.kind === activeKind
                  ? 'bg-primary text-primary-foreground'
                  : 'hover:bg-accent hover:text-accent-foreground')
              }
            >
              <span className="text-sm font-medium">{r.title}</span>
              <span className="flex flex-wrap gap-1">
                {r.verbs.map((v) => (
                  <Badge key={v} variant="outline" className="text-[10px]">
                    {v}
                  </Badge>
                ))}
              </span>
            </button>
          </li>
        ))}
      </ul>
      {resources.length === 0 && (
        <p className="px-4 py-2 text-sm text-muted-foreground">manifest 未暴露任何资源</p>
      )}

      <div className="mt-2 flex items-center justify-between px-4 py-2 text-xs font-medium uppercase tracking-wide text-muted-foreground">
        <span>资源 · 实时</span>
        <span
          className={
            'rounded-full px-1.5 py-0.5 text-[10px] ' +
            (live ? 'bg-green-100 text-green-700' : 'bg-amber-100 text-amber-700')
          }
          title="资源快照的刷新状态"
        >
          {live ? '已同步' : '轮询中'}
        </span>
      </div>
      <ul className="flex flex-col gap-1 px-2 pb-4">
        {vms.map((vm) => (
          <li key={vm.id}>
            <button
              type="button"
              onClick={() => onOpenVm(vm.id)}
              className="flex w-full items-center justify-between rounded-md px-3 py-2 text-left text-sm hover:bg-accent hover:text-accent-foreground"
            >
              <span className="font-mono">VM #{vm.id}</span>
              <StateBadge status={vm.status} />
            </button>
          </li>
        ))}
        {vms.length === 0 && (
          <li className="px-4 py-1 text-sm text-muted-foreground">暂无资源</li>
        )}
      </ul>
    </nav>
  )
}

function statusVariant(status: string): 'default' | 'destructive' | 'outline' {
  // The status is an opaque string: only known terminal states get a colour, unknown
  // values are shown verbatim (degrade, never crash).
  if (status === 'running') return 'default'
  if (status === 'failed') return 'destructive'
  return 'outline'
}

function StateBadge({ status }: { status: string }) {
  const variant = statusVariant(status)
  return (
    <Badge variant={variant} className="text-[10px]">
      {describeStatus(status)}
    </Badge>
  )
}
