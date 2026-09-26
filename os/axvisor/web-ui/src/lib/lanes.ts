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
 * The lanes of one tab to keep connected: its own lanes that no session holds.
 *
 * A tab is the only thing that decides which lanes are connected, so a tab whose
 * lanes are all held opens nothing and the panel reports the tab as blocked.
 * Taking a free lane from some *other* tab instead is what made entering the
 * panel render a merged view nobody asked for: the panel connected a lane the
 * operator was not looking at, and the tab on screen then had to adopt it to
 * show what it had opened.
 *
 * `unavailable` lists lanes this page already lost or released. Two pages can
 * read the same lane as free and race for it, and only the loser learns; without
 * the exclusion the loser would keep retrying the lane it cannot have.
 */
export function lanesToOpen(
  target: readonly string[],
  lanes: readonly LaneOccupancy[],
  unavailable: readonly string[] = [],
): string[] {
  const free = new Set(
    lanes
      .filter((lane) => !lane.attached && !unavailable.includes(lane.route))
      .map((lane) => lane.route),
  )
  return target.filter((route) => free.has(route))
}

/**
 * Index of the first tab holding a lane this page can still connect, `-1` if
 * there is none.
 *
 * Used after losing a race for a lane: the operator must not be left staring at
 * the lane they lost, so the panel moves to a tab it can actually open. The
 * picker is still limited to tabs, which is why the move is a visible tab switch
 * rather than a hidden extra connection.
 */
export function nextFreeTab(
  groups: readonly (readonly string[]) [],
  lanes: readonly LaneOccupancy[],
  unavailable: readonly string[] = [],
): number {
  const free = new Set(
    lanes
      .filter((lane) => !lane.attached && !unavailable.includes(lane.route))
      .map((lane) => lane.route),
  )
  return groups.findIndex((group) => group.some((route) => free.has(route)))
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
