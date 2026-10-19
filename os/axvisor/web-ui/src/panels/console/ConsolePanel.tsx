//! Guest terminals connect only after selection and remain mounted while hidden.
//!
//! The live lane table follows the VM registry. Deletion releases the socket and
//! forgets per-lane state; closing a terminal tab releases its connection without
//! stopping the VM. A failed attach stays on the selected guest for an explicit
//! retry, rather than attaching another guest's lane.

import { useCallback, useEffect, useMemo, useState } from 'react'
import { X } from 'lucide-react'
import { TerminalView } from '@/components/Terminal'
import { describeError, describeStatus, type ConsoleInfo, type PanelProps } from '@/api/types'
import {
  addLanes,
  guestRoute,
  laneRefused,
  lanePhase,
  laneVmId,
  lanesToOpen,
  mergeGroups,
  pruneGoneLanes,
  releaseLane,
  sameSet,
  splitGroup,
  syncGroups,
} from '@/lib/lanes'
import { cn } from '@/lib/utils'

export default function ConsolePanel({
  api,
  link,
  resources = [],
  focusVm = null,
  focusRequest = 0,
  active: visible = true,
}: PanelProps) {
  const [consoles, setConsoles] = useState<ConsoleInfo[] | null>(null)
  const [error, setError] = useState<string | null>(null)
  /**
   * Lanes the operator has actually opened here.
   *
   * Not the same as the lanes that exist: a panel that connected every lane it
   * can see would hold all of them the moment it was mounted, and a second page
   * (or the guest's own serial console) could then never attach to any. A lane
   * joins this list when the tab holding it has been in view, and stays: that is
   * what keeps a visited terminal's buffer alive across tab switches.
   */
  const [visited, setVisited] = useState<string[]>([])
  /** Lanes this page holds, which is the lanes it has opened and kept. */
  const [open, setOpen] = useState<string[]>([])
  /** Lanes whose attach this page lost to another session, so it stops trying. */
  const [failed, setFailed] = useState<string[]>([])
  /** Lanes the operator released here; they stay visible but unconnected. */
  const [released, setReleased] = useState<string[]>([])
  /** One entry per tab; several lanes in one entry means a merged view. */
  const [groups, setGroups] = useState<string[][]>([])
  /**
   * Lanes whose tab the operator closed, so the lane table's next refresh does
   * not hand them straight back. The lane table is the source of truth for which
   * lanes exist, and it still lists a live guest's lane after the tab is gone —
   * a closed tab that came back on the next registry event would make the close
   * control look like it had done nothing.
   */
  const [dismissed, setDismissed] = useState<string[]>([])
  const [active, setActive] = useState(0)
  const [draggedTab, setDraggedTab] = useState<number | null>(null)
  const [consumedFocus, setConsumedFocus] = useState<number | null>(null)

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

  /** Routes a tab may still be built from: everything except what was closed. */
  const liveRoutes = useMemo(
    () => routes.filter((route) => !dismissed.includes(route)),
    [routes, dismissed],
  )

  const laneTable = useMemo(
    () => guestLanes.map((console) => ({ route: console.route, attached: console.attached })),
    [guestLanes],
  )
  const routesKey = routes.join(',')

  // Each console is its own tab; merged tabs survive a table change as long as at
  // least one of their lanes is alive, and a new console arrives as its own tab.
  useEffect(() => {
    setGroups((current) => syncGroups(current, liveRoutes))
  }, [liveRoutes])

  // A lane whose guest is gone is forgotten along with everything decided about
  // it. Deleting a guest removes its lane, and creating one again under the same
  // id makes a *new* lane: it must be offered as a fresh tab rather than staying
  // closed or released because of the guest that used to hold that id.
  useEffect(() => {
    setVisited((current) => pruneGoneLanes(current, routes))
    setFailed((current) => pruneGoneLanes(current, routes))
    setReleased((current) => pruneGoneLanes(current, routes))
    setDismissed((current) => pruneGoneLanes(current, routes))
  }, [routesKey])

  useEffect(() => {
    if (active < groups.length) return
    setActive(Math.max(0, groups.length - 1))
  }, [groups, active])

  // Consume navigation once. It may reopen a dismissed terminal, while later
  // tab switches remain the operator's choice rather than following stale focus.
  useEffect(() => {
    if (!visible || focusVm === null || consumedFocus === focusRequest) return
    const route = guestRoute(focusVm)
    if (!routes.includes(route)) return
    setDismissed((current) => current.includes(route) ? current.filter((lane) => lane !== route) : current)
    const index = groups.findIndex((group) => group.includes(route))
    if (index < 0) return
    setConsumedFocus(focusRequest)
    setReleased((current) => current.filter((lane) => lane !== route))
    setFailed((current) => current.filter((lane) => lane !== route))
    setVisited((current) => addLanes(current, [route]))
    setActive(index)
  }, [visible, focusVm, focusRequest, consumedFocus, routes, groups])

  const target = useMemo(
    () => {
      if (!visible) return []
      if (focusVm !== null && consumedFocus !== focusRequest) {
        const route = guestRoute(focusVm)
        return routes.includes(route) ? [route] : []
      }
      return groups[active] ?? []
    },
    [visible, focusVm, focusRequest, consumedFocus, routes, groups, active],
  )
  const targetKey = target.join('+')

  // Seeing a tab is what opens it. Only a panel that is on screen records this,
  // which is the whole difference between a hidden panel and an unopened one.
  useEffect(() => {
    setVisited((current) => addLanes(current, target))
  }, [targetKey])

  /** Lanes the operator has opened here that still exist, in lane-table order. */
  const connectable = useMemo(
    () => routes.filter((route) => visited.includes(route)),
    [routes, visited],
  )

  useEffect(() => {
    setOpen((current) => {
      // `current` is the held set: the lane table counts this page's own
      // sessions as attached, so held lanes must survive the recomputation
      // instead of being dropped and reconnected on every registry event.
      const next = lanesToOpen(connectable, laneTable, [...failed, ...released], current)
      return sameSet(current, next) ? current : next
    })
  }, [connectable, laneTable, failed, released])

  // Refresh occupancy after an attach failure without selecting another guest.
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

  /**
   * Opens the lane of a tab the operator just asked for.
   *
   * Connecting is a deliberate act here, so this is where a lane nobody has
   * looked at yet joins the opened set — and where a stale "another session has
   * it" is cleared so the table is asked again rather than believed forever.
   */
  const connect = (route: string) => {
    setVisited((current) => addLanes(current, [route]))
    reconnect(route)
  }

  const merge = (from: number, into: number) => {
    // Merging is the operator putting two tabs together, so the merged tab is
    // shown: both lanes are opened, exactly as if each had been visited.
    setVisited((current) => addLanes(current, [...(groups[from] ?? []), ...(groups[into] ?? [])]))
    setGroups((current) => mergeGroups(current, from, into))
    setActive(into > from ? into - 1 : into)
  }

  /** Makes one tab the one in view, which is what opens its lanes. */
  const activate = (index: number) => {
    setActive(index)
    const group = groups[index]
    if (group !== undefined) setVisited((current) => addLanes(current, group))
  }

  const split = (route: string) => {
    setGroups((current) => splitGroup(current, route))
    setActive(groups.length)
  }

  /**
   * Close one tab: let go of every lane it holds and stop offering it again.
   *
   * Closing a tab is not the same as releasing a lane. Releasing keeps the tab
   * and says so, which is what an operator wants for a lane another page may
   * take. Closing says the tab itself is not wanted, so its lanes are released
   * on the way out and then held in `dismissed` — otherwise the next lane table
   * refresh rebuilds the tab from a lane that never went away.
   */
  const closeGroup = (index: number) => {
    const group = groups[index]
    if (group === undefined) return
    setOpen((current) => group.reduce((lanes, route) => releaseLane(lanes, route), current))
    setReleased((current) => [...new Set([...current, ...group])])
    setDismissed((current) => [...new Set([...current, ...group])])
    setGroups((current) => current.filter((_, at) => at !== index))
    load()
  }

  const restoreDismissed = () => {
    setDismissed([])
  }

  const activeGroup = groups[active] ?? []
  const activeOpen = activeGroup.filter((route) => open.includes(route))
  /** Whether the lane table currently reports a session on this route. */
  const occupancy = (route: string) =>
    (consoles ?? []).find((item) => item.route === route)?.attached ?? false
  // "Blocked" is a tab whose every lane is opened here and lost to somebody else:
  // a tab that was never opened is not blocked, it is simply not connected yet,
  // and its panes say so with a way to connect.
  const blocked =
    activeGroup.length > 0 &&
    activeOpen.length === 0 &&
    activeGroup.every(
      (route) =>
        visited.includes(route) &&
        lanePhase(route, {
          open,
          released,
          failed,
          attached: occupancy(route),
        }) === 'occupied',
    )

  /**
   * One lane of one tab.
   *
   * What it shows comes from [`lanePhase`]: a lane this page holds is a
   * terminal; one it released says so and offers 重新连接; one another session
   * holds says that instead of reconnecting, because an exclusive lane would only
   * be refused again; and one that has never been opened offers to connect, so a
   * channel the operator has not asked for is left free for whoever does want it.
   *
   * A connected WebSocket only means the lane is open: input still needs a
   * running guest, and a stopped one drops it silently, so a connected pane whose
   * guest is not running says which of the two is missing instead of looking
   * broken — and an unopened one says it before the operator wastes a click.
   */
  const pane = (route: string, groupPanes: number) => {
    const console = (consoles ?? []).find((item) => item.route === route)
    const vmId = laneVmId(route)
    const vm = vmId === null ? undefined : resources.find((item) => item.id === vmId)
    const label = console?.name ?? route
    const phase = lanePhase(route, {
      open,
      released,
      failed,
      attached: console?.attached ?? false,
    })
    const held = phase === 'connected'
    return (
      <div
        key={route}
        role="group"
        aria-label={route}
        className="relative flex min-h-0 min-w-0 flex-1 flex-col gap-1"
      >
        <div className="flex shrink-0 justify-end gap-1">
          {groupPanes > 1 && (
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
          {phase === 'connected' && (
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
        {held && vm !== undefined && vm.status !== 'running' && (
          <p
            role="status"
            className="shrink-0 rounded-md border border-warn/30 bg-warn/10 px-3 py-2 text-xs text-warn"
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
            {phase === 'released' && (
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
            )}
            {phase === 'occupied' && (
              <>
                <p>
                  这条通道已被另一个会话占用，本页没有连接它。通道是独占订阅：关掉占用它的
                  那处（另一个浏览器页面，或本页另一个终端标签），再点「重试」。
                </p>
                <button
                  type="button"
                  className="rounded border px-2 py-0.5 text-xs hover:bg-accent"
                  onClick={() => reconnect(route)}
                >
                  重试
                </button>
              </>
            )}
            {phase === 'idle' && (
              <>
                <p>
                  这条通道还没有打开，本页只在需要时才占用它。
                  {vm !== undefined &&
                    vm.status !== 'running' &&
                    `客户机「${vm.name}」当前为${describeStatus(vm.status)}，先到「虚拟机」面板启动它，输入才会送达。`}
                </p>
                <button
                  type="button"
                  className="rounded border px-2 py-0.5 text-xs hover:bg-accent"
                  onClick={() => connect(route)}
                >
                  连接
                </button>
              </>
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
                // The wrapper carries the drag gesture and holds two controls side
                // by side: a close control cannot be a child of the button that
                // activates the tab, so the tab is a container rather than one
                // control.
                <div
                  key={group.join('+')}
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
                  className={cn(
                    'flex cursor-grab items-center rounded-md font-mono text-xs',
                    index === active
                      ? 'bg-secondary text-secondary-foreground'
                      : 'text-muted-foreground hover:bg-accent',
                    draggedTab === index && 'opacity-40',
                  )}
                >
                  <button
                    type="button"
                    onClick={() => activate(index)}
                    className="flex items-center gap-1.5 px-2.5 py-1"
                  >
                    <span
                      className={cn(
                        'inline-block h-1.5 w-1.5 rounded-full',
                        held ? 'bg-signal' : 'bg-muted-foreground/40',
                      )}
                    />
                    {label}
                  {/* Held by some session that is not this tab's own lane: either
                      another browser page, or another tab of this panel. */}
                  {busy && (
                    <span
                      className="text-warn"
                      title="该通道已被一个活动会话占用（本页另一个标签，或另一个浏览器页面）"
                    >
                      ●
                    </span>
                  )}
                  </button>
                  {/* Always visible rather than hover-only: a tab that cannot be
                      dismissed from the tab itself leaves the operator with no
                      way to give a lane back without hunting for 「释放」. */}
                  <button
                    type="button"
                    aria-label={`关闭 ${label}`}
                    title={`关闭 ${label}：释放它占用的通道`}
                    onClick={() => closeGroup(index)}
                    className="mr-1.5 rounded p-0.5 text-muted-foreground transition-opacity hover:bg-accent hover:text-foreground opacity-60 hover:opacity-100"
                  >
                    <X className="h-3 w-3" />
                  </button>
                </div>
              )
            })}
            {dismissed.length > 0 && (
              <button
                type="button"
                onClick={restoreDismissed}
                className="rounded px-1.5 py-0.5 text-[11px] text-muted-foreground hover:bg-accent hover:text-foreground"
                title="把关闭过的终端通道重新放回标签栏"
              >
                已关闭 {dismissed.length} 条通道 · 恢复
              </button>
            )}
          </div>
          {blocked && (
            <div className="flex flex-wrap items-center gap-2 rounded-md border border-warn/30 bg-warn/10 px-3 py-2 text-xs text-warn">
              <span>
                这个标签的终端通道都已被占用（本页另一个标签，或另一个浏览器页面）。
                通道为独占订阅，本面板不再抢连；关掉占用它的那处，或点「重试」。
              </span>
              <button
                type="button"
                className="rounded border border-warn/40 px-2 py-0.5 hover:bg-warn/10"
                onClick={retry}
              >
                重试
              </button>
            </div>
          )}
          {/* Every lane is mounted once, hidden ones included: unmounting a
              terminal would drop its buffer, and switching tabs — or merging
              two of them — must not cost the operator the history they already
              read. So the lanes are hoisted out of their tab and keyed by lane
              alone; a tab is only which lanes are visible. Keying the wrappers
              by the tab's contents instead (the obvious `group.join('+')`)
              remounts every terminal in the tab the moment a merge changes
              that string, which is what it used to do. */}
          <div className="flex min-h-0 flex-1 gap-2">
            {groups.flatMap((group, index) => {
              const lanes = group.filter((route) => routes.includes(route))
              return lanes.map((route) => (
                <div
                  key={route}
                  className={cn(
                    'flex min-h-0 min-w-0 flex-1 gap-2',
                    index === active ? 'flex' : 'hidden',
                  )}
                >
                  {pane(route, lanes.length)}
                </div>
              ))
            })}
            {groups[active]?.filter((route) => routes.includes(route)).length === 0 && (
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
