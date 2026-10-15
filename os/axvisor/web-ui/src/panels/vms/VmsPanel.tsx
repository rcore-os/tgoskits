//! Guest management panel: the live registry, the configuration pool it can be
//! started from, and the lifecycle actions on both.
//!
//! The panel keeps three facts apart, because the control plane does too:
//!
//! - the **registry** (`GET /api/vms`) is what is running or ready right now,
//! - the **builtin catalog** (`GET /api/vms/pool`'s `builtin` field) is an immutable
//!   index of packaged configs, while the dynamic **pool** (`entries`) is a
//!   directory of configs that become VMs on demand,
//! - the **detail** (`GET /api/vms/{id}`) carries the counters that prove an
//!   action took effect, so every mutation is followed by a settle poll after the
//!   HTTP response confirms the owner transition.

import { useCallback, useEffect, useRef, useState, type ReactNode } from 'react'
import { Pause, Play, RefreshCw, Square, Terminal, Trash2 } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import {
  ApiError,
  describeError,
  describeStatus,
  type ActionResult,
  type BrowseInfo,
  type PanelProps,
  type PoolInfo,
  type VmDetail,
  type VmStatus,
  type VmSummary,
} from '@/api/types'
import {
  countersOf,
  settleToTerminalState,
  type LifecycleOp,
} from '@/lib/lifecycle'
import { STATUS_DOT, STATUS_TONE } from '@/lib/status'
import { cn } from '@/lib/utils'
import { AffinityMatrix } from './AffinityMatrix'
import { CreateForm } from './CreateForm'
import { DetailDrawer } from './DetailDrawer'
import { Overview } from './Overview'

/** How long fallback registry and pool refreshes wait between requests. */
const REGISTRY_REFRESH_MS = 2000

/**
 * How often the per-guest details are re-read.
 *
 * Slower than the registry, because a detail carries what changes slowly: the
 * affinity mask is fixed for the life of the guest, and the entry counters are
 * read for a trend rather than for a transition — a lifecycle action polls the
 * one guest it is waiting for instead of waiting for this loop.
 */
const DETAIL_REFRESH_MS = 5000

export default function VmsPanel({
  api,
  link,
  files = null,
  resources = [],
  host = null,
  onOpenVm,
}: PanelProps) {
  const [registry, setRegistry] = useState<VmSummary[]>(resources)
  const [pool, setPool] = useState<PoolInfo | null>(null)
  const [poolError, setPoolError] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState<string | null>(null)
  const [note, setNote] = useState<string | null>(null)
  const [detail, setDetail] = useState<VmDetail | null>(null)
  const [createOpen, setCreateOpen] = useState(false)
  const [createToml, setCreateToml] = useState('')
  const [createError, setCreateError] = useState<string | null>(null)
  // The field-driven form, which builds its own request from the declared schema.
  const [formOpen, setFormOpen] = useState(false)
  // Name the pasted config is stored under when it is dropped into the pool.
  const [createName, setCreateName] = useState('')
  const [browseOpen, setBrowseOpen] = useState(false)
  const [browsePath, setBrowsePath] = useState('')
  const [browseInfo, setBrowseInfo] = useState<BrowseInfo | null>(null)
  const [browseError, setBrowseError] = useState<string | null>(null)
  // The guest the last creation made: the panel keeps offering to start it and
  // open its terminal, because that is the rest of the journey the operator
  // started by creating it, and the row's own buttons are easy to lose among the
  // other guests.
  const [created, setCreated] = useState<number | null>(null)
  /**
   * The guest a 启动并打开终端 is in flight for.
   *
   * `busy` already disables every action, but the start and the terminal open
   * are two steps of one gesture: this is what keeps a second click from
   * starting the same guest again or opening a terminal ahead of the settle.
   */
  const pendingOpenRef = useRef<number | null>(null)
  // The guest a 删除 is being confirmed for; `null` when nothing is being deleted.
  const [deleting, setDeleting] = useState<VmSummary | null>(null)
  // Detail of every guest listed, keyed by id. The registry carries neither the
  // affinity mask nor the entry counters, and both are what the overview and
  // the matrix below are drawn from.
  const [details, setDetails] = useState<Record<number, VmDetail>>({})
  // The loop below is keyed on *which* guests exist rather than on the array:
  // the registry is a new array on every poll, and re-reading every detail on
  // each of those would be one request per guest per two seconds.
  const registryRef = useRef(registry)
  registryRef.current = registry
  const registryKey = registry.map((vm) => vm.id).join(',')
  const physCpuCount = host?.phys_cpu_count ?? null
  // The just-created guest as the registry now reports it: the banner's action
  // follows the state it is actually in, not the state it was in at creation.
  const createdVm = created === null ? undefined : registry.find((vm) => vm.id === created)

  const refreshRegistry = useCallback(async () => {
    try {
      const list = await api.get<VmSummary[]>(link.url('list'))
      setRegistry(list)
      setError(null)
    } catch (e: unknown) {
      setError(describeError(e))
    }
  }, [api, link])

  const refreshPool = useCallback(async () => {
    // An API-only build may omit filesystem pool operations entirely: that
    // is a property of this hypervisor, not a failure of a request, so it is
    // asked for by name and no request is sent when it is absent.
    const url = link.maybeUrl('pool')
    if (url === null) {
      setPool(null)
      setPoolError(null)
      return
    }
    try {
      setPool(await api.get<PoolInfo>(url))
      setPoolError(null)
    } catch (e: unknown) {
      setPool(null)
      setPoolError(e instanceof ApiError && e.status === 404 ? null : describeError(e))
    }
  }, [api, link])

  // The event socket is an eager status update path; preserve the metadata the
  // HTTP registry supplied until its next refresh fills a newly created row.
  useEffect(() => {
    setRegistry((previous) => {
      const known = new Map(previous.map((vm) => [vm.id, vm]))
      return resources.map((vm) => {
        const current = known.get(vm.id)
        return current === undefined
          ? vm
          : {
              ...vm,
              cpu_num: vm.cpu_num || current.cpu_num,
              memory_mb: vm.memory_mb || current.memory_mb,
            }
      })
    })
  }, [resources])

  useEffect(() => {
    void refreshPool()
    const poolTimer = link.maybeUrl('pool')
      ? window.setInterval(() => void refreshPool(), REGISTRY_REFRESH_MS)
      : undefined
    return () => {
      if (poolTimer !== undefined) window.clearInterval(poolTimer)
    }
  }, [link, refreshPool])

  useEffect(() => {
    let cancelled = false
    const load = async () => {
      const loaded = await Promise.all(
        registryRef.current.map(async (vm) => {
          try {
            const detail = await api.get<VmDetail>(link.url('detail', { id: vm.id }))
            return { id: vm.id, detail }
          } catch {
            // A guest that went away between the list and this read is simply
            // absent from the matrix; the next poll drops it from both.
            return null
          }
        }),
      )
      if (cancelled) return
      const next: Record<number, VmDetail> = {}
      for (const entry of loaded) if (entry !== null) next[entry.id] = entry.detail
      setDetails(next)
    }
    void load()
    const timer = window.setInterval(() => void load(), DETAIL_REFRESH_MS)
    return () => {
      cancelled = true
      window.clearInterval(timer)
    }
  }, [api, link, registryKey])

  const showDetail = useCallback(
    async (id: number) => {
      try {
        setDetail(await api.get<VmDetail>(link.url('detail', { id })))
      } catch (e: unknown) {
        setNote(describeError(e))
        setDetail(null)
      }
    },
    [api, link],
  )

  /**
   * Runs one mutating operation and then proves it landed.
   *
   * The baseline counters are sampled before the request so the settle poll can
   * require a strict increase; without it a status flip alone would pass for a
   * start that never entered the guest.
   *
   * The answer is whether the operation really settled, which a caller that acts
   * *after* the operation needs: opening a terminal on a start that timed out
   * would attach to a console whose guest never entered.
   */
  const run = useCallback(
    async (
      key: string,
      op: LifecycleOp,
      id: number,
      action: () => Promise<unknown>,
      hint: string,
    ): Promise<boolean> => {
      setBusy(key)
      setNote(hint)
      const detailUrl = link.url('detail', { id })
      try {
        const before = await api
          .get<VmDetail>(detailUrl)
          .then(countersOf)
          .catch(() => ({ guest_entry_count: 0, guest_park_count: 0 }))
        await action()
        const result = await settleToTerminalState(op, before, (signal) =>
          api.get<VmDetail>(detailUrl, signal),
        )
        setNote(result.ok ? `${op} 完成` : result.message)
        if (result.detail) {
          setDetail((current) => current?.id === id ? result.detail ?? null : current)
        }
        await refreshRegistry()
        await refreshPool()
        return result.ok
      } catch (e: unknown) {
        setNote(describeError(e))
        return false
      } finally {
        setBusy(null)
      }
    },
    [api, link, refreshRegistry, refreshPool],
  )

  const start = (id: number) =>
    run(
      `start-${id}`,
      'start',
      id,
      () => api.post<ActionResult>(link.url('start', { id })),
      `启动 VM[${id}]…`,
    )

  const stop = (id: number) =>
    run(`stop-${id}`, 'stop', id, () => api.post<ActionResult>(link.url('stop', { id })), `停止 VM[${id}]…`)

  const pause = (id: number) =>
    run(
      `pause-${id}`,
      'pause',
      id,
      () => api.post<ActionResult>(link.url('pause', { id })),
      `暂停 VM[${id}]…`,
    )

  const resume = (id: number) =>
    run(
      `resume-${id}`,
      'resume',
      id,
      () => api.post<ActionResult>(link.url('resume', { id })),
      `恢复 VM[${id}]…`,
    )

  const remove = (id: number) =>
    run(`delete-${id}`, 'delete', id, () => api.del(link.url('delete', { id })), `删除 VM[${id}]…`)

  /**
   * Start one guest and open its terminal once the start has really landed.
   *
   * The order is the whole point: a console lane exists as soon as the guest is
   * registered, so opening the terminal first would attach to a console whose
   * guest is not running yet and the operator would type into nothing. The
   * terminal is opened only after the settle poll proved a vCPU entered the
   * guest. A start that does not land leaves the reason and the button in place,
   * and pressing it again is the retry.
   */
  const startAndOpen = async (id: number) => {
    if (onOpenVm === undefined || busy !== null || pendingOpenRef.current !== null) return
    pendingOpenRef.current = id
    try {
      if (await start(id)) {
        setCreated(null)
        setNote(`VM[${id}] 已进入 guest，正在打开终端…`)
        onOpenVm(id)
      } else {
        setNote((current) =>
          current === null ? '启动没有完成，可重试' : `${current} · 终端未打开，可重试`,
        )
      }
    } finally {
      pendingOpenRef.current = null
    }
  }

  const create = async () => {
    setCreateError(null)
    setBusy('create')
    try {
      const created = await api.post<{ id: number }>(link.url('create'), { toml: createToml })
      setCreateOpen(false)
      setCreateToml('')
      setCreated(created.id)
      setNote(`已创建 VM[${created.id}]，可用「启动」或「启动并打开终端」进入 guest`)
      await refreshRegistry()
      await refreshPool()
    } catch (e: unknown) {
      // The failure stays inside the dialog: the input is wrong, not the page.
      setCreateError(describeError(e))
    } finally {
      setBusy(null)
    }
  }

  /**
   * Stores the pasted text as a pool file instead of creating a VM from it.
   *
   * The pool is what the hypervisor itself owns, so this is how a config becomes
   * part of the machine: after it is written it is listed here and can be
   * started on demand like any other candidate.
   */
  const saveToPool = async () => {
    setCreateError(null)
    setBusy('save-pool')
    try {
      const saved = await api.post<{ path: string }>(link.url('pool_save'), {
        name: createName.trim(),
        toml: createToml,
      })
      setCreateOpen(false)
      setNote(`已存为候选配置：${saved.path}`)
      await refreshPool()
    } catch (e: unknown) {
      setCreateError(describeError(e))
    } finally {
      setBusy(null)
    }
  }

  /** Reads one directory of the guest filesystem for the folder picker. */
  const loadBrowse = useCallback(
    async (path: string) => {
      setBusy('browse')
      try {
        // The path being browsed is a query parameter, which the manifest does
        // not declare yet: the operation's href covers the route, the panel
        // supplies the argument. A declared query shape would remove this join.
        const url = `${link.url('browse')}?path=${encodeURIComponent(path)}`
        const info = await api.get<BrowseInfo>(url)
        setBrowseInfo(info)
        setBrowsePath(info.path)
        setBrowseError(null)
      } catch (e: unknown) {
        setBrowseInfo(null)
        setBrowseError(describeError(e))
      } finally {
        setBusy(null)
      }
    },
    [api, link],
  )

  const openBrowse = (initial: string) => {
    setBrowseOpen(true)
    setBrowseInfo(null)
    setBrowseError(null)
    void loadBrowse(initial)
  }

  /** Creates a VM from a config file the browser picked, by path. */
  const createFromPath = async (path: string) => {
    setBusy('create-path')
    setBrowseError(null)
    setNote(`从 ${path} 创建客户机…`)
    try {
      const created = await api.post<{ id: number }>(link.url('create'), { path })
      setBrowseOpen(false)
      setCreated(created.id)
      setNote(`已创建 VM[${created.id}]，可用「启动」或「启动并打开终端」进入 guest`)
      await refreshRegistry()
      await refreshPool()
    } catch (e: unknown) {
      // Keep the dialog open on the failure: the picked file is the problem.
      setBrowseError(describeError(e))
    } finally {
      setBusy(null)
    }
  }

  return (
    <div className="flex flex-col gap-4">
      {error && (
        <Banner tone="error">
          读取 VM 列表失败：{error}
          {registry.length > 0 && ' 下表是最后一次成功读取的结果，不代表当前状态。'}
        </Banner>
      )}
      {note && <Banner tone="info">{busy ? `${note}（等待真实状态收敛）` : note}</Banner>}

      {created !== null && (
        <Banner tone="info">
          <span>
            已创建客户机 <span className="font-mono">VM[{created}]</span>。
          </span>
          {/* A status the plane will refuse a start for disables the button, so
              the refusal is not discovered by pressing it; a guest the registry
              does not list yet is left to the request to answer. */}
          {createdVm?.status === 'running' && onOpenVm !== undefined ? (
            <Button
              size="sm"
              variant="outline"
              onClick={() => onOpenVm(created)}
              title="在终端面板里打开这台客户机的通道"
            >
              <Terminal className="mr-1 h-3 w-3" />
              打开终端
            </Button>
          ) : (
            <Button
              size="sm"
              disabled={busy !== null || (createdVm !== undefined && !canStart(createdVm.status))}
              onClick={() =>
                onOpenVm === undefined ? void start(created) : void startAndOpen(created)
              }
              title={
                onOpenVm === undefined
                  ? '启动这台客户机'
                  : '先启动这台客户机，等它真正进入 guest 之后再打开它的终端'
              }
            >
              <Terminal className="mr-1 h-3 w-3" />
              {onOpenVm === undefined ? '启动' : '启动并打开终端'}
            </Button>
          )}
          <Button size="sm" variant="ghost" onClick={() => setCreated(null)}>
            知道了
          </Button>
        </Banner>
      )}

      <Overview vms={registry} details={details} physCpuCount={physCpuCount} />

      <AffinityMatrix vms={registry} details={details} physCpuCount={physCpuCount} />

      <Card>
        <CardHeader className="flex-row items-center justify-between space-y-0">
          <div>
            <CardTitle>已注册客户机</CardTitle>
            <CardDescription>
              使用默认值创建第一台客户机，启动后即可在网页终端输入命令。
            </CardDescription>
          </div>
          <div className="flex gap-2">
            <Button size="sm" variant="outline" onClick={() => void refreshRegistry()}>
              <RefreshCw className="mr-1.5 h-3.5 w-3.5" />
              刷新
            </Button>
            <Button size="sm" onClick={() => setFormOpen(true)}>
              填表创建
            </Button>
            <Button size="sm" variant="outline" onClick={() => setCreateOpen(true)}>
              粘贴配置创建
            </Button>
          </div>
        </CardHeader>
        <CardContent>
          <ul className="flex flex-col gap-1.5">
            {registry.map((vm) => (
              <li
                key={vm.id}
                className="flex flex-wrap items-center gap-x-3 gap-y-2 rounded-lg border px-3 py-2.5 transition-colors hover:bg-accent/40"
              >
                <span
                  className={cn('h-2 w-2 shrink-0 rounded-full', STATUS_DOT[vm.status] ?? 'bg-muted-foreground')}
                  title={describeStatus(vm.status)}
                />
                <span className="font-mono text-xs text-muted-foreground">VM[{vm.id}]</span>
                <span className="min-w-0 flex-1 truncate text-sm font-medium">{vm.name}</span>
                <span
                  className={cn('rounded-full border px-2 py-0.5 text-xs', STATUS_TONE[vm.status] ?? '')}
                >
                  {describeStatus(vm.status)}
                </span>
                <span className="tabular text-xs text-muted-foreground">
                  {vm.cpu_num} vCPU · {vm.memory_mb} MiB
                </span>
                <span className="flex flex-wrap justify-end gap-1">
                  <Button size="sm" variant="ghost" onClick={() => void showDetail(vm.id)}>
                    详情
                  </Button>
                  {canStart(vm.status) && (
                    <Button size="sm" disabled={busy !== null} onClick={() => void start(vm.id)}>
                      <Play className="mr-1 h-3 w-3" />
                      启动
                    </Button>
                  )}
                  {vm.status === 'running' && onOpenVm !== undefined && (
                    <Button
                      size="sm"
                      variant="outline"
                      onClick={() => onOpenVm(vm.id)}
                      title="在终端面板里打开这台客户机的通道（不重新启动它）"
                    >
                      <Terminal className="mr-1 h-3 w-3" />
                      打开终端
                    </Button>
                  )}
                  {canStop(vm.status) && (
                    <Button size="sm" variant="outline" disabled={busy !== null} onClick={() => void stop(vm.id)}>
                      <Square className="mr-1 h-3 w-3" />
                      停止
                    </Button>
                  )}
                  {vm.status === 'running' && (
                    <Button size="sm" variant="outline" disabled={busy !== null} onClick={() => void pause(vm.id)}>
                      <Pause className="mr-1 h-3 w-3" />
                      暂停
                    </Button>
                  )}
                  {vm.status === 'paused' && (
                    <Button size="sm" variant="outline" disabled={busy !== null} onClick={() => void resume(vm.id)}>
                      <Play className="mr-1 h-3 w-3" />
                      恢复
                    </Button>
                  )}
                  <Button
                    size="sm"
                    variant="destructive"
                    disabled={busy !== null}
                    onClick={() => setDeleting(vm)}
                    title="删除这台客户机：先销毁它，再从注册表移除"
                  >
                    <Trash2 className="mr-1 h-3 w-3" />
                    删除客户机
                  </Button>
                </span>
              </li>
            ))}
            {registry.length === 0 && (
              <li className="rounded-lg border border-dashed px-3 py-8 text-center text-sm text-muted-foreground">
                还没有客户机。点击「填表创建」，使用默认值即可开始，无需上传镜像。
              </li>
            )}
          </ul>
        </CardContent>
      </Card>

      <DetailDrawer detail={detail} onClose={() => setDetail(null)} />

      <Dialog
        open={deleting !== null}
        onOpenChange={(open) => {
          if (!open) setDeleting(null)
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>
              删除客户机{deleting !== null && ` VM[${deleting.id}] ${deleting.name}`}
            </DialogTitle>
            <DialogDescription>
              删除会先销毁这台客户机（正在运行的会被停下），并释放它占用的终端通道；注册表里的
              条目随之消失。配置池里的配置不会被删除——需要时用同一份配置重新创建即可。
            </DialogDescription>
          </DialogHeader>
          {deleting !== null && (
            <p className="text-sm text-muted-foreground">
              确定删除 <span className="font-mono">VM[{deleting.id}]</span>（{deleting.name}）吗？
            </p>
          )}
          <DialogFooter>
            <Button variant="outline" onClick={() => setDeleting(null)}>
              取消
            </Button>
            <Button
              variant="destructive"
              disabled={busy !== null}
              onClick={() => {
                // Close first: the dialog is about one guest, and the request it
                // starts outlives it. The row's own state carries the progress.
                const target = deleting
                setDeleting(null)
                if (target !== null) void remove(target.id)
              }}
            >
              删除客户机
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Card>
        <CardHeader className="flex-row items-center justify-between space-y-0">
          <div>
            <CardTitle>内置客户机与动态候选</CardTitle>
            <CardDescription>
              内置客户机随系统提供，动态候选来自已配置的目录。点击「启动」后创建客户机。
            </CardDescription>
          </div>
          {pool && (
            <Button size="sm" variant="outline" onClick={() => openBrowse(pool.directory)}>
              浏览目录…
            </Button>
          )}
        </CardHeader>
        <CardContent className="flex flex-col gap-3 text-sm">
          {poolError && <Banner tone="error">读取候选配置失败：{poolError}</Banner>}
          {pool === null && !poolError && (
            <p className="text-muted-foreground">
              这个构建没有候选配置（未启用文件系统能力），可以直接粘贴配置创建。
            </p>
          )}
          {pool && (
            <>
              <p className="text-muted-foreground">
                读取来源（按优先级）：
                {pool.sources.map((source) => (
                  <span key={source} className="ml-1 font-mono text-xs">
                    {source}
                  </span>
                ))}
              </p>
              <p className="text-muted-foreground">
                新建配置写入：
                <span className="ml-1 font-mono text-xs">{pool.directory}</span>
              </p>
              <section className="flex flex-col gap-2">
                <h3 className="font-medium">内置客户机（只读）</h3>
                <ul className="flex flex-col gap-2">
                  {pool.builtin.map((entry) => (
                    <li key={`builtin-${entry.id}-${entry.path}`} className="rounded border p-2">
                      <div className="flex items-center justify-between gap-2">
                        <span>
                          VM[{entry.id}] <span className="font-mono text-xs">{entry.name}</span>
                        </span>
                        <span className="flex items-center gap-2">
                          <span className="font-mono text-xs text-muted-foreground">{entry.path}</span>
                          <CandidateAction
                            entryId={entry.id}
                            registry={registry}
                            busy={busy !== null}
                            onOpenVm={onOpenVm}
                            onStart={() => void start(entry.id)}
                            onStartAndOpen={() => void startAndOpen(entry.id)}
                          />
                        </span>
                      </div>
                    </li>
                  ))}
                  {pool.builtin.length === 0 && (
                    <li className="text-muted-foreground">这个构建没有 builtin 客户机配置。</li>
                  )}
                </ul>
              </section>
              <section className="flex flex-col gap-2">
                <h3 className="font-medium">动态池</h3>
                <ul className="flex flex-col gap-2">
                  {pool.entries.map((entry) => (
                    <li key={`${entry.id}-${entry.path}`} className="rounded border p-2">
                      <div className="flex items-center justify-between gap-2">
                        <span>
                          VM[{entry.id}] <span className="font-mono text-xs">{entry.name}</span>
                        </span>
                        <span className="flex items-center gap-2">
                          <span className="font-mono text-xs text-muted-foreground">{entry.path}</span>
                          <CandidateAction
                            entryId={entry.id}
                            registry={registry}
                            busy={busy !== null}
                            onOpenVm={onOpenVm}
                            onStart={() => void start(entry.id)}
                            onStartAndOpen={() => void startAndOpen(entry.id)}
                          />
                        </span>
                      </div>
                    </li>
                  ))}
                  {pool.entries.length === 0 && (
                    <li className="text-muted-foreground">
                      写入目录里还没有可启动的配置，用「粘贴配置创建」里的「存为候选配置」放一份进去。
                    </li>
                  )}
                </ul>
              </section>
              <ul className="flex flex-col gap-1">
                {pool.issues.map((issue) => (
                  <li key={`${issue.kind}-${issue.path}`} className="text-xs text-warn">
                    无法使用 <span className="font-mono">{issue.path}</span>（{issue.kind}）：{issue.detail}
                  </li>
                ))}
              </ul>
            </>
          )}
        </CardContent>
      </Card>

      <Dialog open={browseOpen} onOpenChange={setBrowseOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>从目录里选配置创建客户机</DialogTitle>
            <DialogDescription>
              逐层列目录；每个 `.toml` 都已经解析过，能启动的才给「创建」，不能启动的在下面
              写明原因。
            </DialogDescription>
          </DialogHeader>
          <div className="flex flex-col gap-2 text-sm">
            <div className="flex items-center gap-2">
              <Input
                value={browsePath}
                onChange={(event) => setBrowsePath(event.target.value)}
                spellCheck={false}
                className="font-mono text-xs"
              />
              <Button
                size="sm"
                variant="outline"
                disabled={busy !== null || browsePath.trim().length === 0}
                onClick={() => void loadBrowse(browsePath.trim())}
              >
                打开
              </Button>
              <Button
                size="sm"
                variant="outline"
                disabled={busy !== null || !browseInfo?.parent}
                onClick={() => browseInfo?.parent && void loadBrowse(browseInfo.parent)}
              >
                上级
              </Button>
            </div>
            {browseError && <Banner tone="error">{browseError}</Banner>}
            {browseInfo && (
              <>
                <ul className="flex flex-col gap-1">
                  {browseInfo.directories.map((directory) => (
                    <li key={directory.path}>
                      <button
                        type="button"
                        className="font-mono text-xs text-left hover:underline"
                        onClick={() => void loadBrowse(directory.path)}
                      >
                        [dir] {directory.path}
                      </button>
                    </li>
                  ))}
                  {browseInfo.entries.map((entry) => (
                    <li
                      key={entry.path}
                      className="flex items-center justify-between gap-2 rounded border p-2"
                    >
                      <span className="font-mono text-xs">{entry.path}</span>
                      <span className="flex items-center gap-2">
                        <span className="text-xs text-muted-foreground">
                          VM[{entry.id}] {entry.name}
                        </span>
                        <Button
                          size="sm"
                          disabled={busy !== null}
                          onClick={() => void createFromPath(entry.path)}
                        >
                          创建
                        </Button>
                      </span>
                    </li>
                  ))}
                  {browseInfo.directories.length === 0 && browseInfo.entries.length === 0 && (
                    <li className="text-muted-foreground">这个目录里没有子目录，也没有可用的配置。</li>
                  )}
                </ul>
                <ul className="flex flex-col gap-1">
                  {browseInfo.issues.map((issue) => (
                    <li key={`${issue.kind}-${issue.path}`} className="text-xs text-warn">
                      无法使用 <span className="font-mono">{issue.path}</span>（{issue.kind}）：{issue.detail}
                    </li>
                  ))}
                </ul>
              </>
            )}
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setBrowseOpen(false)}>
              关闭
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog open={createOpen} onOpenChange={setCreateOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>粘贴客户机配置</DialogTitle>
            <DialogDescription>
              「创建」用这份 TOML 建一台客户机；「存为候选配置」把它写进候选配置卡片显示的
              目录，写进去的配置会出现在动态池里，之后可以直接启动。
            </DialogDescription>
          </DialogHeader>
          <textarea
            className="h-56 w-full rounded-md border bg-transparent p-2 font-mono text-xs"
            value={createToml}
            spellCheck={false}
            placeholder={'[base]\nid = 9\nname = "guest"\n\n[kernel]\nkernel_path = "/guest/kernel"\n...'}
            onChange={(event) => setCreateToml(event.target.value)}
          />
          {pool && (
            <Input
              value={createName}
              onChange={(event) => setCreateName(event.target.value)}
              spellCheck={false}
              className="font-mono text-xs"
              placeholder="文件名，如 guest.toml（写入 /guest）"
            />
          )}
          {createError && <Banner tone="error">{createError}</Banner>}
          <DialogFooter>
            <Button variant="outline" onClick={() => setCreateOpen(false)}>
              取消
            </Button>
            {pool && (
              <Button
                variant="outline"
                disabled={busy !== null || createToml.trim().length === 0 || createName.trim().length === 0}
                onClick={() => void saveToPool()}
              >
                存为候选配置
              </Button>
            )}
            <Button disabled={busy !== null || createToml.trim().length === 0} onClick={() => void create()}>
              创建
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <CreateForm
        api={api}
        link={link}
        files={files}
        open={formOpen}
        onOpenChange={setFormOpen}
        onCreated={(id, saved) => {
          // A form-made guest is registered the same way a pool-made one is and
          // its configuration is written to the guest tree, so both lists are
          // refreshed rather than assumed.
          setCreated(id)
          setNote(saved ? `已创建 VM[${id}]，候选配置：${saved}` : `已创建 VM[${id}]`)
          void refreshRegistry()
          void refreshPool()
        }}
      />
    </div>
  )
}

/**
 * Which actions the current status allows.
 *
 * The control plane is the authority — it answers 409 for a transition the state
 * machine rejects — but the buttons should not offer a start that is known to be
 * refused: restart-after-stop is explicitly rejected (a fresh vCPU task on an
 * idled pinned CPU never gets scheduled), so `stopped` has no start button.
 */
export function canStart(status: VmStatus): boolean {
  return status === 'ready'
}

export function canStop(status: VmStatus): boolean {
  return status === 'running' || status === 'paused'
}

/**
 * The actions one configuration-pool candidate offers.
 *
 * A candidate is a config, not a guest: starting it is what creates the guest,
 * so a candidate whose id is already in the registry is not a candidate to start
 * again. The second 启动 would create a VM that is already there (or be refused),
 * and either way the operator meant the running guest — so the row names its
 * state and offers the terminal instead.
 */
function CandidateAction({
  entryId,
  registry,
  busy,
  onOpenVm,
  onStart,
  onStartAndOpen,
}: {
  entryId: number
  registry: VmSummary[]
  busy: boolean
  onOpenVm: ((id: number) => void) | undefined
  onStart(): void
  onStartAndOpen(): void
}) {
  const registered = registry.find((vm) => vm.id === entryId)
  if (registered !== undefined) {
    return (
      <>
        <span className="text-xs text-muted-foreground">
          已注册（{describeStatus(registered.status)}），不再重复启动
        </span>
        {registered.status === 'running' && onOpenVm !== undefined && (
          <Button
            size="sm"
            variant="outline"
            onClick={() => onOpenVm(registered.id)}
            title="在终端面板里打开这台客户机的通道"
          >
            <Terminal className="mr-1 h-3 w-3" />
            打开终端
          </Button>
        )}
      </>
    )
  }
  return (
    <>
      <Button size="sm" disabled={busy} onClick={onStart}>
        启动
      </Button>
      {onOpenVm !== undefined && (
        <Button
          size="sm"
          variant="outline"
          disabled={busy}
          onClick={onStartAndOpen}
          title="启动这台候选配置，等它真正进入 guest 之后再打开它的终端"
        >
          <Terminal className="mr-1 h-3 w-3" />
          启动并打开终端
        </Button>
      )}
    </>
  )
}

function Banner({ tone, children }: { tone: 'info' | 'error'; children: ReactNode }) {
  return (
    <p
      className={cn(
        'flex flex-wrap items-center gap-2 rounded-md border px-3 py-2 text-sm',
        tone === 'error' ? 'border-destructive/40 bg-destructive/10' : 'bg-muted',
      )}
    >
      {children}
    </p>
  )
}
