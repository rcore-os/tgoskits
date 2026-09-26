//! Guest terminal panel: one tab per console, and tabs merge by dragging.
//!
//! The lane table is *runtime state*: `GET /api/consoles` is derived from the VM
//! registry, so a lane appears when a VM is created and disappears when it is
//! closed. The panel therefore re-reads the table whenever the registry feed
//! reports a change — that is the whole reason the shell injects the feed here —
//! and drops the terminals whose lane is gone instead of retrying them forever.
//!
//! Every console is its own tab, and a tab connects only its own lanes. Entering
//! the panel therefore opens at most the lane of the tab the operator lands on —
//! never a free lane belonging to some other tab, which is how a merged view
//! nobody asked for used to appear. Dragging one tab onto another merges them:
//! both lanes are then shown side by side in one tab, and each pane of a merged
//! tab offers 「分离」 to go back to its own tab. This is the same gesture the demo
//! uses, and the reason the split is not a separate mode: what is shown together
//! is what was put together.
//!
//! Lanes are exclusive on the host, so only the *active* tab's lanes are
//! connected. Mounting every tab would hold every lane at once and starve any
//! other page; the layout therefore never decides occupancy, and a lane that was
//! released is shown as released rather than silently reconnected.

import { useCallback, useEffect, useMemo, useState } from 'react'
import { TerminalView } from '@/components/Terminal'
import { describeError, describeStatus, type ConsoleInfo, type PanelProps } from '@/api/types'
import {
  guestRoute,
  laneRefused,
  laneVmId,
  lanesToOpen,
  mergeGroups,
  nextFreeTab,
  releaseLane,
  sameSet,
  splitGroup,
  syncGroups,
} from '@/lib/lanes'
import { cn } from '@/lib/utils'

export default function ConsolePanel({ api, link, resources = [], focusVm = null }: PanelProps) {
  const [consoles, setConsoles] = useState<ConsoleInfo[] | null>(null)
  const [error, setError] = useState<string | null>(null)
  /** Lanes this page holds, which is the active tab's lanes. */
  const [open, setOpen] = useState<string[]>([])
  /** Lanes whose attach this page lost to another session, so it stops trying. */
  const [failed, setFailed] = useState<string[]>([])
  /** Lanes the operator released here; they stay visible but unconnected. */
  const [released, setReleased] = useState<string[]>([])
  /** One entry per tab; several lanes in one entry means a merged view. */
  const [groups, setGroups] = useState<string[][]>([])
  const [active, setActive] = useState(0)
  const [draggedTab, setDraggedTab] = useState<number | null>(null)

  const load = useCallback(() => {
    let cancelled = false
    api
      .get<ConsoleInfo[]>(link.url('list'))
      .then((list) => {
        if (cancelled) return
        setConsoles(list)
        setError(null)
      })
      .catch((e: unknown) => {
        if (cancelled) return
        setConsoles([])
        setError(describeError(e))
      })
    return () => {
      cancelled = true
    }
  }, [api, link])

  // `resources` changes identity on every registry event, so this is a fetch per
  // change and nothing more: no timer, no polling.
  useEffect(() => load(), [load, resources])

  // Guest lanes only: the management shell has its own panel, so showing it here
  // as well would put the same exclusive lane in two places — and a tab for it
  // could be merged into a guest tab, which is one more way to end up with a view
  // nobody asked for.
  const guestLanes = useMemo(
    () => (consoles ?? []).filter((console) => console.route !== 'axvisor'),
    [consoles],
  )

  const routes = useMemo(() => guestLanes.map((console) => console.route), [guestLanes])

  const laneTable = useMemo(
    () => guestLanes.map((console) => ({ route: console.route, attached: console.attached })),
    [guestLanes],
  )

  // Each console is its own tab; merged tabs survive a table change as long as at
  // least one of their lanes is alive, and a new console arrives as its own tab.
  useEffect(() => {
    setGroups((current) => syncGroups(current, routes))
  }, [routes])

  useEffect(() => {
    if (active < groups.length) return
    setActive(Math.max(0, groups.length - 1))
  }, [groups, active])

  /** Lanes the active tab wants, in tab order. */
  const target = useMemo(
    () => (groups[active] ?? []).filter((route) => routes.includes(route)),
    [groups, active, routes],
  )

  useEffect(() => {
    setOpen((current) => {
      const next = lanesToOpen(target, laneTable, [...failed, ...released])
      return sameSet(current, next) ? current : next
    })
  }, [target, laneTable, failed, released])

  /**
   * After losing a race for a lane, move to a tab whose lane can be opened.
   *
   * The tab is the only thing that decides what is connected, so a lost race is
   * answered by switching tabs instead of quietly connecting another lane: the
   * operator sees where they ended up, and the tab they were on stays a tab.
   * When there is nothing free to move to, nothing happens and the blocked
   * banner reports it.
   */
  useEffect(() => {
    if (failed.length === 0) return
    const index = nextFreeTab(groups, laneTable, [...failed, ...released])
    if (index >= 0) setActive(index)
  }, [failed, groups, laneTable, released])

  useEffect(() => {
    if (focusVm === null) return
    const route = guestRoute(focusVm)
    const index = groups.findIndex((group) => group.includes(route))
    if (index >= 0) setActive(index)
  }, [focusVm, groups])

  /**
   * A lane whose socket closed without opening may have been lost to another
   * page, and the table this panel holds is exactly the one that looked free
   * when it chose the lane. So re-read the table and decide on *that*: a lane the
   * new table reports as held is dropped from its tab and the picker moves on,
   * while a lane that still looks free is left alone (the close was something
   * else).
   */
  const attempt = useCallback(
    (route: string) => {
      api
        .get<ConsoleInfo[]>(link.url('list'))
        .then((list) => {
          setConsoles(list)
          setError(null)
          const table = list
            .filter((item) => item.route !== 'axvisor')
            .map((item) => ({ route: item.route, attached: item.attached }))
          if (!laneRefused(table, route)) return
          setFailed((current) => (current.includes(route) ? current : [...current, route]))
          setOpen((current) => releaseLane(current, route))
          // The lane is not this page's to keep: take it out of the tab so the
          // operator sees the tab they can actually use.
          setGroups((current) =>
            current
              .map((group) => group.filter((lane) => lane !== route))
              .filter((group) => group.length > 0),
          )
        })
        // The backend is unreachable: keep the lane and its state as they are,
        // the terminal itself already reports the lost connection.
        .catch(() => undefined)
    },
    [api, link],
  )

  const retry = () => {
    setFailed([])
    setReleased([])
    load()
  }

  const release = (route: string) => {
    setOpen((current) => releaseLane(current, route))
    setReleased((current) => (current.includes(route) ? current : [...current, route]))
    load()
  }

  const reconnect = (route: string) => {
    setReleased((current) => current.filter((lane) => lane !== route))
    setFailed((current) => current.filter((lane) => lane !== route))
    load()
  }

  const merge = (from: number, into: number) => {
    setGroups((current) => mergeGroups(current, from, into))
    setActive(into > from ? into - 1 : into)
  }

  const split = (route: string) => {
    setGroups((current) => splitGroup(current, route))
    setActive(groups.length)
  }

  const activeGroup = groups[active] ?? []
  const panes = activeGroup.filter((route) => routes.includes(route))
  const blocked = open.length === 0 && panes.some((route) => !released.includes(route))

  /**
   * One lane of the active tab. A connected WebSocket only means the lane is
   * open: input still needs a running guest, and a stopped one drops it silently,
   * so the pane says which of the two is missing instead of looking broken.
   */
  const pane = (route: string) => {
    const console = (consoles ?? []).find((item) => item.route === route)
    const vmId = laneVmId(route)
    const vm = vmId === null ? undefined : resources.find((item) => item.id === vmId)
    const held = open.includes(route)
    const label = console?.name ?? route
    return (
      <div
        key={route}
        role="group"
        aria-label={route}
        className="relative flex min-h-0 min-w-0 flex-1 flex-col gap-1"
      >
        <div className="absolute right-1 top-1 z-10 flex items-center gap-1">
          {panes.length > 1 && (
            <button
              type="button"
              aria-label={`分离 ${label}`}
              title="把这个终端分离出去，回到它自己的标签"
              className="rounded bg-background/80 px-1.5 py-0.5 text-[10px] text-muted-foreground hover:bg-accent hover:text-foreground"
              onClick={() => split(route)}
            >
              分离
            </button>
          )}
          {held && (
            <button
              type="button"
              aria-label={`释放 ${label} 通道`}
              title="释放这条通道：断开本页对它的占用，让其他页面可以连接"
              className="rounded bg-background/80 px-1.5 py-0.5 text-[10px] text-muted-foreground hover:bg-accent hover:text-foreground"
              onClick={() => release(route)}
            >
              释放
            </button>
          )}
        </div>
        {vm !== undefined && vm.status !== 'running' && (
          <p
            role="status"
            className="shrink-0 rounded-md border border-amber-300 bg-amber-50 px-3 py-2 text-xs text-amber-800"
          >
            客户机「{vm.name}」当前为{describeStatus(vm.status)}：终端已连上通道，
            但输入不会送达客户机。先到「虚拟机」面板点「启动」，再回到这里输入。
          </p>
        )}
        {held ? (
          <TerminalView
            path={link.url('stream', { endpoint: route })}
            title={label}
            subtitle={link.url('stream', { endpoint: route })}
            occupied={console?.attached}
            onClosed={() => attempt(route)}
            className="min-h-0 flex-1"
          />
        ) : (
          <div className="flex min-h-0 flex-1 flex-col items-center justify-center gap-2 rounded-md border text-sm text-muted-foreground">
            {released.includes(route) ? (
              <>
                <p>这条通道已由本页释放，当前没有连接。</p>
                <button
                  type="button"
                  className="rounded border px-2 py-0.5 text-xs hover:bg-accent"
                  onClick={() => reconnect(route)}
                >
                  重新连接
                </button>
              </>
            ) : (
              <p>这条通道已被另一个会话占用，本页没有连接它。</p>
            )}
          </div>
        )}
      </div>
    )
  }

  return (
    <div className="flex h-[70vh] min-h-0 flex-col gap-2">
      {error && (
        <p className="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm">
          读取终端清单失败：{error}
        </p>
      )}
      {guestLanes.length === 0 ? (
        <p className="rounded-md border px-3 py-6 text-center text-sm text-muted-foreground">
          当前没有客户机终端通道。浏览器的终端通道随客户机创建而出现、随关闭而释放，
          先到「虚拟机」面板启动一台。
        </p>
      ) : (
        <>
          <div
            className="flex shrink-0 flex-wrap items-center gap-1"
            onDragOver={(event) => {
              if (draggedTab !== null) event.preventDefault()
            }}
          >
            {groups.map((group, index) => {
              const label =
                group.length > 1
                  ? group.map((route) => route.replace(/^vm-/, '#')).join('+')
                  : (guestLanes.find((item) => item.route === group[0])?.name ?? group[0])
              const held = group.some((route) => open.includes(route))
              const busy = group.some(
                (route) =>
                  !open.includes(route) &&
                  (guestLanes.find((item) => item.route === route)?.attached ?? false),
              )
              return (
                <button
                  key={group.join('+')}
                  type="button"
                  draggable
                  title="拖动这个标签到另一个标签上可以融合为同屏分列"
                  onDragStart={(event) => {
                    event.dataTransfer.setData('text/plain', String(index))
                    event.dataTransfer.effectAllowed = 'move'
                    setDraggedTab(index)
                  }}
                  onDragEnd={() => setDraggedTab(null)}
                  onDragOver={(event) => {
                    if (draggedTab === null) return
                    event.preventDefault()
                  }}
                  onDrop={(event) => {
                    event.preventDefault()
                    event.stopPropagation()
                    if (draggedTab !== null && draggedTab !== index) merge(draggedTab, index)
                    setDraggedTab(null)
                  }}
                  onClick={() => setActive(index)}
                  className={cn(
                    'flex cursor-grab items-center gap-1.5 rounded-md px-2.5 py-1 font-mono text-xs',
                    index === active
                      ? 'bg-secondary text-secondary-foreground'
                      : 'text-muted-foreground hover:bg-accent',
                    draggedTab === index && 'opacity-40',
                  )}
                >
                  <span
                    className={cn(
                      'inline-block h-1.5 w-1.5 rounded-full',
                      held ? 'bg-emerald-500' : 'bg-muted-foreground/40',
                    )}
                  />
                  {label}
                  {/* Held by some session that is not this tab's own lane: either
                      another browser page, or another tab of this panel. */}
                  {busy && (
                    <span
                      className="text-amber-500"
                      title="该通道已被一个活动会话占用（本页另一个标签，或另一个浏览器页面）"
                    >
                      ●
                    </span>
                  )}
                </button>
              )
            })}
          </div>
          {blocked && (
            <div className="flex flex-wrap items-center gap-2 rounded-md border border-amber-300 bg-amber-50 px-3 py-2 text-xs text-amber-800">
              <span>
                这个标签的终端通道都已被占用（本页另一个标签，或另一个浏览器页面）。
                通道为独占订阅，本面板不再抢连；关掉占用它的那处，或点「重试」。
              </span>
              <button
                type="button"
                className="rounded border border-amber-400 px-2 py-0.5 hover:bg-amber-100"
                onClick={retry}
              >
                重试
              </button>
            </div>
          )}
          {/* Only the active tab's lanes stay mounted: the lanes are exclusive, so
              mounting every tab would hold every one of them. */}
          <div className="flex min-h-0 flex-1 gap-2">
            {panes.map((route) => pane(route))}
            {panes.length === 0 && (
              <p className="flex flex-1 items-center justify-center text-sm text-muted-foreground">
                这个标签没有可连接的通道。
              </p>
            )}
          </div>
        </>
      )}
    </div>
  )
}
