//! The figures an operator looks at first.
//!
//! The registry below answers "which guests exist and what can I do to them";
//! these four answer "what is this machine carrying", which is the question a
//! row list cannot: the vCPU and memory totals are properties of the *set*, and
//! the overcommit ratio exists only because the host has a finite number of
//! cores that the set can exceed.

import type { ReactNode } from 'react'
import { Boxes, Cpu, Gauge, MemoryStick } from 'lucide-react'
import type { VmDetail, VmSummary } from '@/api/types'
import { formatMemory, overcommit, ratio } from '@/lib/format'
import { cn } from '@/lib/utils'

export interface OverviewProps {
  vms: VmSummary[]
  /** Per-guest detail, keyed by id; a guest not read yet is simply not counted. */
  details: Record<number, VmDetail>
  /** Physical CPUs the host reports, `null` before the host panel has read it. */
  physCpuCount: number | null
}

export function Overview({ vms, details, physCpuCount }: OverviewProps) {
  const running = vms.filter((vm) => vm.status === 'running').length
  const vcpus = vms.reduce((sum, vm) => sum + vm.cpu_num, 0)
  const memory = vms.reduce((sum, vm) => sum + vm.memory_mb, 0)
  // The counters are the proof that a start actually reached the guest, so
  // their total is the one figure here that says "these guests are running"
  // rather than "these guests are configured to be running".
  const entries = Object.values(details).reduce(
    (sum, detail) => sum + (detail.guest_entry_count ?? 0),
    0,
  )

  return (
    <div className="grid gap-px overflow-hidden rounded-lg border bg-border sm:grid-cols-2 xl:grid-cols-4">
      <Kpi
        icon={<Boxes className="h-3.5 w-3.5" />}
        label="客户机"
        value={`${vms.length}`}
        unit="台"
        foot={`${running} 台运行中`}
      />
      <Kpi
        icon={<Cpu className="h-3.5 w-3.5" />}
        label="已分配 vCPU"
        value={`${vcpus}`}
        unit="核"
        foot={
          physCpuCount === null
            ? '宿主核数未知'
            : `宿主 ${physCpuCount} 核 · 超配比 ${overcommit(vcpus, physCpuCount)}×`
        }
        bar={physCpuCount === null ? null : ratio(vcpus, physCpuCount)}
        barTone={physCpuCount !== null && vcpus > physCpuCount ? 'over' : 'normal'}
      />
      <Kpi
        icon={<MemoryStick className="h-3.5 w-3.5" />}
        label="已分配内存"
        value={formatMemory(memory)}
        foot="宿主内存总量不由控制面导出"
      />
      <Kpi
        icon={<Gauge className="h-3.5 w-3.5" />}
        label="进入 guest 次数"
        value={`${entries}`}
        unit="次"
        foot="只在 vCPU 真的进入 guest 后增长"
      />
    </div>
  )
}

function Kpi({
  icon,
  label,
  value,
  unit,
  foot,
  bar = null,
  barTone = 'normal',
}: {
  icon: ReactNode
  label: string
  value: string
  unit?: string
  foot: string
  /** Fraction of the track to fill, or `null` for "no denominator known". */
  bar?: number | null
  barTone?: 'normal' | 'over'
}) {
  return (
    // The four figures are one instrument, not four cards: they share a single
    // hairline grid, so what separates them is a rule and not a gap of page.
    <div className="bg-card p-4">
      <div className="flex items-center gap-1.5 eyebrow">
        <span className="text-muted-foreground/70">{icon}</span>
        {label}
      </div>
      <div className="mt-2 flex items-baseline gap-1">
        <span className="figure text-[26px] font-semibold leading-none">{value}</span>
        {unit && <span className="text-xs text-muted-foreground">{unit}</span>}
      </div>
      {bar !== null && (
        <div className="mt-3 h-[3px] w-full overflow-hidden rounded-full bg-muted">
          <div
            className={cn(
              'h-full rounded-full transition-[width] duration-300',
              barTone === 'over' ? 'bg-warn' : 'bg-primary',
            )}
            style={{ width: `${Math.round(bar * 100)}%` }}
          />
        </div>
      )}
      <p className="mt-2.5 text-[11px] leading-snug text-muted-foreground">{foot}</p>
    </div>
  )
}
