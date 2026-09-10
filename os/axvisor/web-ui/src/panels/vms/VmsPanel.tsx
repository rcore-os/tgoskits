//! vms 面板 —— 纯管理页：列表、创建、生命周期动作、删除。
//!
//! 语义纪律：HTTP 200 只代表请求被接受，终态由 `@/lib/lifecycle` 轮询详情断言
//! （start/resume 要 guest_entry_count 增长、pause 要 guest_park_count 增长）。
//! 路由全部从 `meta.href` 派生，不硬编码端点。

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

/** 动作前的零基线：stop/delete 不需要计数增长证明，只做状态断言。 */
const NO_COUNTERS: Counters = { guest_entry_count: 0, guest_park_count: 0 }

/**
 * 创建对话框的默认模板：一份 `image_location = "memory"` 的 guest 配置。
 * 运行期只能实例化构建期内嵌的镜像，且只按 `base.id` 匹配，所以这份模板与
 * `test-suit/axvisor/normal/qemu-web-ui/web-ui/vm-memory.toml` 的 `base.id` 一致；
 * 改成别的 id 会因为找不到内嵌镜像而返回 500（后端既有行为）。
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

/** 行内可用动作：非法动作不渲染，而不是点了才报 409。 */
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
      // 后端不支持重启已停止的 VM（start 会 409）：只能删除后重新创建。
      return [{ op: 'delete', label: '删除', destructive: true }]
    default:
      // failed / destroying / 未知状态：只读展示，等待后端收敛
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
  // 危险操作（create/delete）要求重输 token：这是确认式摩擦，不是安全边界——
  // token 本来就在这个浏览器里。校验复用 api/auth.ts 的同一原语。
  const [retyped, setRetyped] = useState('')
  const [verifying, setVerifying] = useState(false)
  const [confirmError, setConfirmError] = useState<string | null>(null)

  /**
   * 危险操作前的 token 复核。
   * 后端未声明校验端点（auth 缺失）时退化为放行，并把这句话写在对话框里。
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

  // 资源根来自 manifest：详情/动作路由都从它派生（href + "/{id}" 等）。
  const base = meta.href
  const fetchDetail = (id: number) => api.get<VmDetail>(`${base}/${id}`)

  const runAction = async (op: ActionName, id: number) => {
    setConfirming(null)
    setBusy({ id, op })
    setError(null)
    try {
      // start/resume 要证明计数增长，pause 要证明 park 增长，所以先采基线。
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
      // 409 在创建场景下的含义是「这个 id 已经被占用」，通用状态码文案说不清楚。
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
            // 行级 busy：既用于防重（按钮 disabled），也用于显示当前动作
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
                        // stop/delete 是破坏性且不可撤销的，先二次确认
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
                  // 删除不可逆：先让后端复核一次重输的 token 再执行。
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
