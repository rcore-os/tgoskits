//! One guest, read out of `GET /api/vms/{id}`.
//!
//! The counters are the point of this view and the reason it is not a card
//! under the list: `guest_entry_count` advances only after a vCPU really
//! re-entered the guest and `guest_park_count` only after one really parked, so
//! they are the evidence that a start or a pause did something — which a status
//! string alone never is, because it flips before the guest counters settle.

import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { describeStatus, type VmDetail } from '@/api/types'
import { describeCpuAffinity } from '@/lib/vcpu'
import { STATUS_DOT, STATUS_TONE } from '@/lib/status'
import { cn } from '@/lib/utils'

export interface DetailDrawerProps {
  /** The guest to show; `null` closes the drawer. */
  detail: VmDetail | null
  onClose: () => void
}

export function DetailDrawer({ detail, onClose }: DetailDrawerProps) {
  return (
    <Dialog open={detail !== null} onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="max-w-2xl">
        {detail && (
          <>
            <DialogHeader>
              <DialogTitle className="flex flex-wrap items-center gap-2 font-mono text-base">
                VM[{detail.id}]
                <span className="font-sans text-sm font-normal text-muted-foreground">
                  {detail.name}
                </span>
                <span
                  className={cn(
                    'flex items-center gap-1.5 rounded-full border px-2 py-0.5 text-xs font-normal',
                    STATUS_TONE[detail.status] ?? '',
                  )}
                >
                  <span className={cn('h-1.5 w-1.5 rounded-full', STATUS_DOT[detail.status] ?? '')} />
                  {describeStatus(detail.status)}
                </span>
              </DialogTitle>
              <DialogDescription>
                {detail.cpu_num} vCPU · {detail.memory_mb} MiB · 计数只在 vCPU 真的进入或 park
                之后增长
              </DialogDescription>
            </DialogHeader>

            <div className="grid grid-cols-2 gap-3">
              <Counter label="guest_entry_count" value={detail.guest_entry_count} note="进入 guest" />
              <Counter
                label="guest_park_count"
                value={detail.guest_park_count}
                note="park 于挂起等待"
              />
            </div>

            <div>
              <p className="mb-1.5 text-xs text-muted-foreground">
                vCPU 状态与物理核亲和（{(detail.vcpu_states ?? []).length} 个）
              </p>
              {(detail.vcpu_states ?? []).length === 0 ? (
                <p className="text-sm text-muted-foreground">这个构建没有导出 vCPU 状态。</p>
              ) : (
                <ul className="grid gap-1.5 sm:grid-cols-2">
                  {(detail.vcpu_states ?? []).map((vcpu) => (
                    <li
                      key={vcpu.id}
                      className="flex items-center justify-between gap-2 rounded-md border px-2.5 py-1.5 text-xs"
                    >
                      <span className="font-mono">vCPU {vcpu.id}</span>
                      <span className="flex items-center gap-2">
                        <span className="text-muted-foreground">{vcpu.state}</span>
                        <span className="font-mono">{describeCpuAffinity(vcpu.phys_cpu_set)}</span>
                      </span>
                    </li>
                  ))}
                </ul>
              )}
            </div>
          </>
        )}
      </DialogContent>
    </Dialog>
  )
}

function Counter({
  label,
  value,
  note,
}: {
  label: string
  value: number | undefined
  note: string
}) {
  return (
    <div className="rounded-lg border bg-muted/40 px-3 py-2.5">
      <div className="font-mono text-[11px] text-muted-foreground">{label}</div>
      <div className="tabular mt-0.5 text-xl font-semibold leading-none">{value ?? '—'}</div>
      <div className="mt-1 text-[11px] text-muted-foreground">{note}</div>
    </div>
  )
}
