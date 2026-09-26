//! Colour tones for the VM lifecycle status.
//!
//! Kept in one place so the navigation list and the management panel cannot
//! disagree about what "running" looks like, and so a status this build does not
//! know falls back to a neutral tone instead of an unstyled badge.

import type { VmStatus } from '@/api/types'

export const STATUS_TONE: Record<string, string> = {
  ready: 'border-sky-300 bg-sky-50 text-sky-700',
  running: 'border-emerald-300 bg-emerald-50 text-emerald-700',
  pausing: 'border-amber-300 bg-amber-50 text-amber-700',
  paused: 'border-amber-300 bg-amber-50 text-amber-700',
  stopping: 'border-amber-300 bg-amber-50 text-amber-700',
  stopped: 'border-zinc-300 bg-zinc-50 text-zinc-600',
  destroying: 'border-amber-300 bg-amber-50 text-amber-700',
  destroyed: 'border-zinc-300 bg-zinc-50 text-zinc-600',
  failed: 'border-red-300 bg-red-50 text-red-700',
  unknown: 'border-zinc-300 bg-zinc-50 text-zinc-600',
} satisfies Partial<Record<VmStatus | string, string>>
