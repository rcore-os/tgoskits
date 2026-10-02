//! Number presentation shared by the panels.
//!
//! The control plane reports raw units — seconds, MiB — and every panel that
//! shows them has to agree on what they read like, so the conversion lives here
//! rather than in each panel. Nothing here talks to the backend.

/** Seconds as `3d 04:05`, `02:05:09` or `00:09`: the unit that is not zero leads. */
export function formatUptime(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) return '—'
  const total = Math.floor(seconds)
  const days = Math.floor(total / 86400)
  const hours = Math.floor((total % 86400) / 3600)
  const minutes = Math.floor((total % 3600) / 60)
  const secs = total % 60
  const pad = (value: number) => String(value).padStart(2, '0')
  if (days > 0) return `${days}d ${pad(hours)}:${pad(minutes)}`
  return `${pad(hours)}:${pad(minutes)}:${pad(secs)}`
}

/**
 * A memory size in MiB, as GiB once it is large enough to read better that way.
 *
 * The switch is at 1024 MiB exactly, so `1023 MiB` stays in MiB and `1024 MiB`
 * becomes `1.0 GiB` — a figure that is shown next to another one should not
 * change unit between two refreshes of a single MiB.
 */
export function formatMemory(mib: number): string {
  if (!Number.isFinite(mib) || mib < 0) return '—'
  if (mib < 1024) return `${Math.round(mib)} MiB`
  return `${(mib / 1024).toFixed(1)} GiB`
}

/**
 * `used` as a fraction of `total`, clamped to `[0, 1]`.
 *
 * A guest may be configured with more vCPUs than the machine has, so the ratio
 * a bar is drawn from can exceed one; the bar cannot be longer than the track,
 * and the caller shows the overcommit as a number next to it.
 */
export function ratio(used: number, total: number): number {
  if (!Number.isFinite(used) || !Number.isFinite(total) || total <= 0) return 0
  return Math.min(1, Math.max(0, used / total))
}

/** `used / total` with one decimal, e.g. `1.5×`; `0` when there is no total. */
export function overcommit(used: number, total: number): number {
  if (!Number.isFinite(used) || !Number.isFinite(total) || total <= 0) return 0
  return Math.round((used / total) * 10) / 10
}
