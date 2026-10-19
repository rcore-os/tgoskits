//! Console-lane naming shared by the panels.
//!
//! `network_console::layout` names a guest lane `vm-<id>` and keeps the
//! management lane at `axvisor`. Both directions live here so the console panel
//! and any future caller agree on how a lane is derived from a VM and how a VM
//! is recovered from a lane.

/** Route of the guest lane that belongs to `vmId`. */
export function guestRoute(vmId: number): string {
  return `vm-${vmId}`
}

/**
 * VM id of a guest lane route, `null` for the management lane.
 *
 * Returns `null` rather than throwing: routes come from the control plane, and
 * an unfamiliar lane should be shown without a claim about which VM it serves.
 */
export function laneVmId(route: string): number | null {
  const match = /^vm-(\d+)$/.exec(route)
  if (match === null) return null
  const id = Number(match[1])
  return Number.isSafeInteger(id) ? id : null
}

/** Occupancy of one lane as the control plane last reported it. */
export interface LaneOccupancy {
  route: string
  /** A session is attached to the lane, which may be this page's own. */
  attached: boolean
}

/**
 * Lanes a console panel holds open, which are the lanes of one tab.
 *
 * The lanes are exclusive on the host, so a panel must not connect every lane it
 * can see: entering the panel would then hold all of them at once and a second
 * page could never attach to any. Opening is therefore scoped to the tab the
 * operator is looking at, and [`lanesToOpen`] answers "which of *this tab's*
 * lanes are free", never "which lane could this page grab".
 */

/** Drops `route` from the open set, releasing the host-side session. */
export function releaseLane(open: readonly string[], route: string): string[] {
  return open.filter((lane) => lane !== route)
}

/**
 * `visited` with `lanes` added, or the very same array when it already holds them.
 *
 * Returning the same reference when nothing is added is what lets an effect
 * record "the tab in view has been seen" on every render without scheduling
 * another one. What the list means is *what the operator has opened*, which is
 * why it is not derived from the lane table: the table says which lanes exist,
 * and existence alone must not open one.
 */
export function addLanes(visited: string[], lanes: readonly string[]): string[] {
  const added = lanes.filter((lane) => !visited.includes(lane))
  return added.length === 0 ? visited : [...visited, ...added]
}

/**
 * `lanes` without the routes that no longer exist.
 *
 * A lane whose guest was deleted is gone, and so is everything remembered about
 * it: a guest created again under the same id gets a *new* lane, which must not
 * inherit "the operator closed this tab" or "another session took it" from the
 * one before — that guest was never opened, and the operator has no way to tell
 * why its terminal refuses to appear.
 */
export function pruneGoneLanes(lanes: readonly string[], routes: readonly string[]): string[] {
  return lanes.filter((lane) => routes.includes(lane))
}

/** What one lane's pane shows. */
export type LanePhase = 'connected' | 'released' | 'occupied' | 'idle'

/**
 * What one lane is showing, from the facts the panel holds about it.
 *
 * `open` is this page's own connections, `released` the lanes it explicitly gave
 * back, `failed` the lanes it lost to another session, and `attached` the lane
 * table's answer — which counts this page's own sessions too, and that is why a
 * held lane is connected whatever the table says. Anything else the table
 * reports as attached is somebody else's session (another page, or another
 * terminal tab of this panel), and a lane nobody holds and this page never
 * opened is simply not connected yet.
 */
export function lanePhase(
  route: string,
  state: {
    open: readonly string[]
    released: readonly string[]
    failed: readonly string[]
    attached: boolean
  },
): LanePhase {
  if (state.open.includes(route)) return 'connected'
  if (state.released.includes(route)) return 'released'
  if (state.failed.includes(route) || state.attached) return 'occupied'
  return 'idle'
}

/**
 * The lanes to keep connected: the target's lanes, plus the lanes this page
 * already holds. The caller's target is the lanes the operator has actually
 * opened here — never "every lane this panel could connect", which is what would
 * let a mounted-but-hidden panel hold a lane nobody asked for.
 *
 * A tab is the only thing that decides which lanes are connected, so a tab whose
 * lanes are all held opens nothing and the panel reports the tab as blocked.
 * Taking a free lane from some *other* tab instead is what made entering the
 * panel render a merged view nobody asked for: the panel connected a lane the
 * operator was not looking at, and the tab on screen then had to adopt it to
 * show what it had opened.
 *
 * `held` lists lanes this page's own sessions are attached to. The lane table
 * counts those as attached too, so they are kept open on that basis alone —
 * dropping them would close a working terminal on every registry event. Only
 * `unavailable` (lost races and explicit releases) and a lane another session
 * holds take a lane out of the open set.
 *
 * `unavailable` lists lanes this page already lost or released. Two pages can
 * read the same lane as free and race for it, and only the loser learns; without
 * the exclusion the loser would keep retrying the lane it cannot have.
 */
export function lanesToOpen(
  target: readonly string[],
  lanes: readonly LaneOccupancy[],
  unavailable: readonly string[] = [],
  held: readonly string[] = [],
): string[] {
  const free = new Set(
    lanes
      .filter((lane) => !lane.attached && !unavailable.includes(lane.route))
      .map((lane) => lane.route),
  )
  return target.filter(
    (route) =>
      !unavailable.includes(route) && (held.includes(route) || free.has(route)),
  )
}

/**
 * Whether a lane that failed to attach is held by some other session.
 *
 * A browser WebSocket reports every refused handshake as an anonymous close, so
 * the lane table is what tells a lost race apart from a backend that went away.
 * The caller releases the lane first: a lane held by another session is not
 * "open" here, and keeping it open would leave the panel stuck on the one lane
 * it cannot have.
 */
export function laneRefused(lanes: readonly LaneOccupancy[], route: string): boolean {
  return lanes.some((lane) => lane.route === route && lane.attached)
}

/**
 * Whether two lane lists hold the same lanes, ignoring order.
 *
 * An effect that recomputes a lane set must return its previous value unchanged
 * when nothing differs, otherwise every render schedules another one.
 */
export function sameSet(left: readonly string[], right: readonly string[]): boolean {
  return left.length === right.length && left.every((lane) => right.includes(lane))
}

/**
 * The tab groups after a lane table change.
 *
 * A tab group is one tab: a single lane is an ordinary tab, several lanes are a
 * merged view shown side by side. A lane that appears gets its own tab (so every
 * console is reachable, and none is connected until its tab is active); a lane
 * whose VM is gone is dropped from its group; a group that loses its last lane
 * disappears, and a merged group keeps its remaining lanes together.
 */
export function syncGroups(
  groups: readonly (readonly string[]) [],
  routes: readonly string[],
): string[][] {
  const kept = groups
    .map((group) => group.filter((route) => routes.includes(route)))
    .filter((group) => group.length > 0)
  const known = new Set(kept.flat())
  const added = routes.filter((route) => !known.has(route)).map((route) => [route])
  return [...kept, ...added]
}

/**
 * The tab groups after dragging tab `from` onto tab `into`.
 *
 * This is the merge gesture: the dropped tab's lanes join the tab it landed on,
 * and only that tab remains. Lanes are never listed twice, and the merged view
 * keeps the target's lanes first so the tab does not visually reshuffle.
 */
export function mergeGroups(
  groups: readonly (readonly string[]) [],
  from: number,
  into: number,
): string[][] {
  if (from === into || from < 0 || from >= groups.length || into < 0 || into >= groups.length) {
    return groups.map((group) => [...group])
  }
  const moved = groups[from].filter((route) => !groups[into].includes(route))
  const target = into > from ? into - 1 : into
  return groups
    .filter((_, index) => index !== from)
    .map((group, index) => (index === target ? [...group, ...moved] : [...group]))
}

/**
 * The tab groups after splitting `route` out of its merged tab.
 *
 * The lane becomes its own tab at the end, which is where a newly split view
 * belongs: the merged tab keeps the other lanes, and the operator ends up
 * looking at the lane they split out.
 */
export function splitGroup(
  groups: readonly (readonly string[]) [],
  route: string,
): string[][] {
  if (!groups.some((group) => group.includes(route))) {
    return groups.map((group) => [...group])
  }
  const rest = groups
    .map((group) => group.filter((lane) => lane !== route))
    .filter((group) => group.length > 0)
  return [...rest, [route]]
}
