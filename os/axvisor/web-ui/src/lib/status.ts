//! Colour tones for the VM lifecycle status.
//!
//! Kept in one place so the navigation list and the management panel cannot
//! disagree about what "running" looks like, and so a status this build does not
//! know falls back to a neutral tone instead of an unstyled badge.
//!
//! Every tone is named for what it means — `signal` for a guest that is up,
//! `warn` for one that is mid-transition, `alert` for one that failed — and
//! each is one CSS variable with a value per colour scheme. That is why a tone
//! is written once here rather than twice: a tinted fill with dark text is
//! unreadable under the dark scheme, and the variable is what keeps the two
//! schemes apart instead of a second copy of every class.

import type { VmStatus } from '@/api/types'

const NEUTRAL = 'border-border bg-muted text-muted-foreground'
const LIVE = 'border-signal/30 bg-signal/10 text-signal'
const MOVING = 'border-warn/30 bg-warn/10 text-warn'
const BROKEN = 'border-alert/30 bg-alert/10 text-alert'

export const STATUS_TONE: Record<string, string> = {
  ready: NEUTRAL,
  running: LIVE,
  pausing: MOVING,
  paused: MOVING,
  stopping: MOVING,
  stopped: NEUTRAL,
  destroying: MOVING,
  destroyed: NEUTRAL,
  failed: BROKEN,
  unknown: NEUTRAL,
} satisfies Partial<Record<VmStatus | string, string>>

/**
 * Fill of the status dot next to a guest.
 *
 * A dot carries the state where a badge would cost a line of its own — in the
 * guest list, where every row already repeats the same few words. Transitional
 * states pulse, because "stopping" that has been stopping for a minute is a
 * different thing to look into than a guest that is simply stopped.
 */
export const STATUS_DOT: Record<string, string> = {
  ready: 'bg-muted-foreground',
  running: 'bg-signal',
  pausing: 'bg-warn animate-pulse',
  paused: 'bg-warn',
  stopping: 'bg-warn animate-pulse',
  stopped: 'bg-muted-foreground',
  destroying: 'bg-warn animate-pulse',
  destroyed: 'bg-muted-foreground',
  failed: 'bg-alert',
  unknown: 'bg-muted-foreground',
} satisfies Partial<Record<VmStatus | string, string>>
