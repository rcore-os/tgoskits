//! Which vCPU of which guest is pinned to which physical core.
//!
//! The registry says a guest has four vCPUs; it does not say whether two guests
//! are fighting over one core. The detail of each guest does say it —
//! `phys_cpu_set` is a per-vCPU bitmask — so this view is a join of the
//! details the panel already reads, drawn as one cell per (guest, core) pair.
//!
//! An unpinned vCPU is not drawn as a column: it has no core to mark, and
//! marking every core for it would say the opposite of the truth.

import type { VmDetail, VmSummary } from '@/api/types'
import { decodeCpuSet } from '@/lib/vcpu'
import { cn } from '@/lib/utils'

export interface AffinityMatrixProps {
  vms: VmSummary[]
  details: Record<number, VmDetail>
  physCpuCount: number | null
}

/** Beyond this many cores the grid stops being readable, so it stops growing. */
const MAX_COLUMNS = 64

export function AffinityMatrix({ vms, details, physCpuCount }: AffinityMatrixProps) {
  const rows = vms
    .map((vm) => {
      const detail = details[vm.id]
      const vcpus = detail?.vcpu_states ?? []
      const cores = new Set<number>()
      let pinned = 0
      for (const vcpu of vcpus) {
        const set = decodeCpuSet(vcpu.phys_cpu_set)
        if (set.length === 0) continue
        pinned += 1
        for (const core of set) cores.add(core)
      }
      return { vm, cores, pinned, total: vcpus.length }
    })
    // A guest whose detail has not been read yet has nothing to draw; showing
    // it as a row of empty cells would read as "this guest is unpinned".
    .filter((row) => row.total > 0)

  const highest = rows.reduce((max, row) => Math.max(max, ...row.cores), -1)
  const columns = Math.min(
    MAX_COLUMNS,
    Math.max(1, physCpuCount ?? 0, highest + 1),
  )
  const columnIndexes = Array.from({ length: columns }, (_, index) => index)

  if (rows.length === 0) {
    return null
  }

  return (
    <div className="rounded-lg border bg-card p-3.5">
      <div className="flex items-baseline justify-between gap-3">
        <span className="text-xs text-muted-foreground">
          vCPU 亲和矩阵 · 行是客户机，列是物理核
        </span>
        <span className="flex items-center gap-3 text-[11px] text-muted-foreground">
          <Legend className="bg-primary" label="已绑定" />
          <Legend className="bg-muted" label="空闲/未绑定" />
        </span>
      </div>
      <div className="mt-3 overflow-x-auto">
        <div className="min-w-fit">
          <div
            className="grid gap-1"
            style={{ gridTemplateColumns: `minmax(5rem, 8rem) repeat(${columns}, 1.25rem)` }}
          >
            <span />
            {columnIndexes.map((core) => (
              <span key={core} className="text-center font-mono text-[10px] text-muted-foreground">
                {core}
              </span>
            ))}
          </div>
          {rows.map((row) => (
            <div
              key={row.vm.id}
              className="mt-1 grid gap-1"
              style={{ gridTemplateColumns: `minmax(5rem, 8rem) repeat(${columns}, 1.25rem)` }}
            >
              <span className="truncate font-mono text-xs text-muted-foreground">
                VM[{row.vm.id}]
              </span>
              {columnIndexes.map((core) => (
                <span
                  key={core}
                  title={
                    row.cores.has(core)
                      ? `VM[${row.vm.id}] 有 vCPU 绑定在 Core ${core}`
                      : `Core ${core}：VM[${row.vm.id}] 未占用`
                  }
                  className={cn(
                    'h-5 rounded-[3px] border',
                    row.cores.has(core)
                      ? 'border-transparent bg-primary'
                      : 'border-border bg-muted',
                  )}
                />
              ))}
            </div>
          ))}
        </div>
      </div>
      <p className="mt-2.5 text-[11px] leading-snug text-muted-foreground">
        {rows.filter((row) => row.pinned < row.total).length > 0 &&
          '有未绑定的 vCPU，它们不出现在矩阵里：未绑定意味着由宿主调度，而不是占有某一列。'}
      </p>
    </div>
  )
}

function Legend({ className, label }: { className: string; label: string }) {
  return (
    <span className="flex items-center gap-1.5">
      <span className={cn('h-2.5 w-2.5 rounded-[2px]', className)} />
      {label}
    </span>
  )
}
