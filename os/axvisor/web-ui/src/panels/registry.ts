//! Renderer registry: kind -> lazily loaded component.
//!
//! Invariant 6: adding a panel means a new panels/ directory, one line here, and one
//! manifest node — three places in total, with the shell, routes and client unchanged.
//! Panels never import each other: vms (management page) and console (terminal host)
//! are independent components that only share the resource event stream injected by
//! the shell. That is "composable building blocks, not welded together".

import { lazy } from 'react'
import type { PanelComponent, PanelRegistry } from '@/api/types'
import { FallbackPanel } from './FallbackPanel'

// Lazy loading: having a kind in the registry does not mean the user opened it —
// the chunk is downloaded only when it is actually used.
const VmsPanel = lazy(() => import('./vms/VmsPanel'))

const renderers: Record<string, PanelComponent> = {
  vms: VmsPanel, // VM management page: create / lifecycle / list
}

export function resolvePanel(kind: string): PanelComponent {
  return renderers[kind] ?? FallbackPanel
}

export const panelRegistry: PanelRegistry = { resolve: resolvePanel }
