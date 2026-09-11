//! vms panel — a pure management page: list, create, lifecycle actions, delete.
//!
//! Semantics: HTTP 200 only means the request was accepted; the terminal state is
//! asserted by `@/lib/lifecycle` polling the detail (start/resume require
//! guest_entry_count to grow, pause requires guest_park_count to grow).
//! Every route is derived from `meta.href`; no endpoint is hardcoded.

import { useState } from 'react'
import { verifyToken } from '@/api/auth'
import {
  ApiError,
  describeError,
  describeStatus,
  type PanelProps,
  type VmDetail,
} from '@/api/types'
import {
  countersOf,
  settleToTerminalState,
  type Counters,
  type LifecycleOp,
} from '@/lib/lifecycle'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'

/** Zero baseline for actions that only assert a status: stop/delete need no counter growth. */
const NO_COUNTERS: Counters = { guest_entry_count: 0, guest_park_count: 0 }

/**
 * Default template for the create dialog: a guest config with
 * `image_location = "memory"`.
 * At runtime only images embedded at build time can be instantiated, matched by
 * `base.id` alone, so this template keeps the same `base.id` as
 * `test-suit/axvisor/normal/qemu-web-ui/web-ui/vm-memory.toml`; any other id returns
 * 500 because no embedded image matches (existing backend behaviour).
 */
const DEFAULT_VM_TOML = `[base]
id = 1
name = "linux-web-ui"
guest_type = "passthrough"
cpu_num = 1
phys_cpu_ids = [1]

[kernel]
entry_point = 0x8020_0000
image_location = "memory"
kernel_path = "\${workspace}/tmp/axbuild/images/qemu-aarch64/linux/linux-qemu"
kernel_load_addr = 0x8020_0000
dtb_load_addr = 0x8000_0000
ramdisk_path = "\${workspace}/tmp/axbuild/axvisor/qemu-web-ui/initramfs-aarch64.cpio.gz"
ramdisk_load_addr = 0x8400_0000

memory_regions = [
  [0x8000_0000, 0x1000_0000, 0x7, 0],
]

[devices]
passthrough = []
# The host's own PCI bridge (and with it virtio-net) must stay with the host.
disabled = [{ path = "/pcie@10000000" }]
`

type ActionName = Exclude<LifecycleOp, 'create'>

interface RowAction {
  op: ActionName
  label: string
  destructive?: boolean
}

/** Actions available per row: illegal actions are not rendered, rather than returning 409 only once clicked. */
function actionsFor(status: string): RowAction[] {
  switch (status) {
    case 'ready':
      return [
        { op: 'start', label: '启动' },
        { op: 'delete', label: '删除', destructive: true },
      ]
    case 'running':
      return [
        { op: 'pause', label: '暂停' },
        { op: 'stop', label: '停止' },
        { op: 'delete', label: '删除', destructive: true },
      ]
    case 'paused':
      return [
        { op: 'resume', label: '恢复' },
        { op: 'stop', label: '停止' },
        { op: 'delete', label: '删除', destructive: true },
      ]
    case 'stopped':
      // The backend cannot restart a stopped VM (start returns 409): delete and
      // recreate is the only path.
      return [{ op: 'delete', label: '删除', destructive: true }]
    default:
      // failed / destroying / unknown: read-only display, waiting for the backend to converge.
      return []
  }
}

export default function VmsPanel({ meta, api, resources = [], refresh, auth }: PanelProps) {
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState<{ id: number; op: LifecycleOp } | null>(null)
  const [confirming, setConfirming] = useState<{ op: ActionName; id: number } | null>(null)
  const [createOpen, setCreateOpen] = useState(false)
  // A create failure must be readable while the dialog holds the screen: the
  // card's own banner sits behind the overlay, so it would look like nothing
  // happened.
  const [createError, setCreateError] = useState<string | null>(null)
  const [toml, setToml] = useState(DEFAULT_VM_TOML)
  // Dangerous operations (create/delete) require retyping the token: this is
  // confirmation friction, not a security boundary — the token is already in this
  // browser. Verification reuses the same primitive from api/auth.ts.
  const [retyped, setRetyped] = useState('')
  const [verifying, setVerifying] = useState(false)
  const [confirmError, setConfirmError] = useState<string | null>(null)

  /**
   * Re-verifies the token before a dangerous operation.
   * When the backend declares no probe endpoint (auth missing) this degrades to
   * letting it through, and the dialog says so.
   */
  const verifyRetyped = async (report: (message: string) => void): Promise<boolean> => {
    if (!auth) return true
    const candidate = retyped.trim()
    if (!candidate) {
      report('请重新输入 token')
      return false
    }
    setVerifying(true)
    const verdict = await verifyToken(auth, candidate)
    setVerifying(false)
    if (verdict.ok) return true
    report(verdict.reason)
    return false
  }

  // The resource root comes from the manifest: detail and action routes are derived
  // from it (href + "/{id}" and so on).
  const base = meta.href
  const fetchDetail = (id: number) => api.get<VmDetail>(`${base}/${id}`)

  const runAction = async (op: ActionName, id: number) => {
    setConfirming(null)
    setBusy({ id, op })
    setError(null)
    try {
      // start/resume must prove the entry counter grew and pause must prove park grew,
      // so the baseline is sampled first.
      const before = op === 'stop' ? NO_COUNTERS : countersOf(await fetchDetail(id))
      if (op === 'delete') {
        await api.delete(`${base}/${id}`)
      } else {
        await api.post(`${base}/${id}/${op}`)
      }
      const result = await settleToTerminalState(op, before, () => fetchDetail(id))
      if (!result.ok) setError(result.message)
    } catch (e: unknown) {
      setError(describeError(e))
    } finally {
      setBusy(null)
      refresh?.()
    }
  }

  const runCreate = async () => {
    setCreateError(null)
    if (!(await verifyRetyped(setCreateError))) return
    setBusy({ id: -1, op: 'create' })
    try {
      const created = await api.post<{ id: number }>(`${base}/create`, { toml })
      setCreateOpen(false)
      const result = await settleToTerminalState('create', NO_COUNTERS, () =>
        fetchDetail(created.id),
      )
      if (!result.ok) setError(result.message)
    } catch (e: unknown) {
      // A 409 in the create case means "this id is already taken", which the generic
      // status text cannot convey.
      setCreateError(
        e instanceof ApiError && e.status === 409
          ? '该 id 已被占用：先删除现有 VM，或换一个构建期内嵌了镜像的 id'
          : describeError(e),
      )
    } finally {
      setBusy(null)
      refresh?.()
    }
  }

  return (
    <Card className="max-w-3xl">
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          虚拟机
          <Badge variant="secondary">{resources.length} 台</Badge>
        </CardTitle>
        <CardDescription>
          每个操作都轮询到真实终态：启动/恢复以 guest_entry_count 增长为证，暂停以
          guest_park_count 增长为证。
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-3">
        <div className="flex items-center gap-2">
          <Button
            size="sm"
            disabled={busy !== null}
            onClick={() => {
              setCreateError(null)
              setRetyped('')
              setCreateOpen(true)
            }}
          >
            创建 VM
          </Button>
          {error && <span className="font-mono text-xs text-destructive">{error}</span>}
        </div>

        <ul className="divide-y rounded-md border">
          {resources.map((vm) => {
            const actions = actionsFor(vm.status)
            // Row-level busy: used both to prevent double submission (button disabled)
            // and to show the active action.
            const busyHere = busy !== null && busy.id === vm.id ? busy : null
            return (
              <li key={vm.id} className="flex items-center justify-between gap-2 px-3 py-2">
                <span className="flex items-center gap-2 font-mono text-sm">
                  VM #{vm.id}
                  <Badge
                    variant={vm.status === 'running' ? 'default' : 'outline'}
                    className="text-[10px]"
                  >
                    {describeStatus(vm.status)}
                  </Badge>
                  {vm.status === 'stopped' && (
                    <span className="text-xs text-muted-foreground">
                      已停止的 VM 不能重启，请删除后重新创建
                    </span>
                  )}
                </span>
                <span className="flex items-center gap-2">
                  {busyHere !== null && (
                    <span className="text-xs text-muted-foreground">{busyHere.op} 进行中…</span>
                  )}
                  {actions.map((action) => (
                    <Button
                      key={action.op}
                      size="sm"
                      disabled={busy !== null}
                      variant={action.destructive ? 'destructive' : 'outline'}
                      onClick={() => {
                        // stop/delete are destructive and irreversible, so ask for a
                        // second confirmation.
                        if (action.op === 'stop' || action.op === 'delete') {
                          setRetyped('')
                          setConfirmError(null)
                          setConfirming({ op: action.op, id: vm.id })
                        } else {
                          void runAction(action.op, vm.id)
                        }
                      }}
                    >
                      {action.label}
                    </Button>
                  ))}
                </span>
              </li>
            )
          })}
          {resources.length === 0 && (
            <li className="px-3 py-2 text-sm text-muted-foreground">
              还没有 VM——点「创建 VM」
            </li>
          )}
        </ul>
      </CardContent>

      <Dialog
        open={confirming !== null}
        onOpenChange={(open) => {
          if (!open) setConfirming(null)
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>
              确认{confirming?.op === 'delete' ? '删除' : '停止'} VM #{confirming?.id}？
            </DialogTitle>
            <DialogDescription>
              {confirming?.op === 'delete'
                ? '删除后该 VM 从列表消失；要再次使用需重新创建。'
                : '停止是异步操作：状态会先进入 stopping，等 vCPU 退出后才到达 stopped。'}
            </DialogDescription>
          </DialogHeader>

          {confirming?.op === 'delete' && auth && (
            <div className="space-y-1">
              <label className="text-xs text-muted-foreground" htmlFor="confirm-token">
                删除不可逆：请重新输入管理 token 确认
              </label>
              <Input
                id="confirm-token"
                type="password"
                value={retyped}
                onChange={(e) => {
                  setRetyped(e.target.value)
                  setConfirmError(null)
                }}
                placeholder="Bearer token"
                aria-label="确认删除的 token"
              />
              {confirmError && (
                <p className="font-mono text-xs text-destructive" role="alert">
                  {confirmError}
                </p>
              )}
            </div>
          )}

          <DialogFooter>
            <Button variant="outline" onClick={() => setConfirming(null)}>
              取消
            </Button>
            <Button
              variant="destructive"
              disabled={verifying}
              onClick={() => {
                if (!confirming) return
                const target = confirming
                void (async () => {
                  // Delete is irreversible: have the backend re-verify the retyped
                  // token before running it.
                  if (target.op === 'delete') {
                    setConfirmError(null)
                    if (!(await verifyRetyped(setConfirmError))) return
                  }
                  await runAction(target.op, target.id)
                })()
              }}
            >
              确认
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog open={createOpen} onOpenChange={setCreateOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>创建 VM</DialogTitle>
            <DialogDescription>
              运行期只能实例化构建期内嵌的镜像（按 base.id 匹配）。改 id 或路径会让请求
              返回 500。
            </DialogDescription>
          </DialogHeader>
          <textarea
            className="h-64 w-full resize-y rounded-md border bg-muted/40 p-2 font-mono text-xs"
            aria-label="创建 VM 的 TOML 配置"
            value={toml}
            onChange={(e) => setToml(e.target.value)}
          />
          {auth && (
            <div className="space-y-1">
              <label className="text-xs text-muted-foreground" htmlFor="create-token">
                创建会占用资源：请重新输入管理 token 确认
              </label>
              <Input
                id="create-token"
                type="password"
                value={retyped}
                onChange={(e) => {
                  setRetyped(e.target.value)
                  setCreateError(null)
                }}
                placeholder="Bearer token"
                aria-label="确认创建的 token"
              />
            </div>
          )}
          {createError && (
            <p className="font-mono text-xs text-destructive" role="alert">
              {createError}
            </p>
          )}
          <DialogFooter>
            <Button variant="outline" onClick={() => setCreateOpen(false)}>
              取消
            </Button>
            <Button
              disabled={busy !== null || verifying || !toml.trim()}
              onClick={() => void runCreate()}
            >
              {verifying ? '校验中…' : '创建'}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </Card>
  )
}
