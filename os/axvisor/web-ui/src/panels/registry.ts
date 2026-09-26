//! Renderer registry: `kind` → lazily loaded component.
//!
//! Adding a panel means a new directory under `panels/`, one line here, and one
//! node in the backend manifest — three places, with the shell, the navigation
//! and the client untouched. Panels never import each other: they only share the
//! request client and the registry snapshot the shell injects.

import { lazy } from 'react'
import type { PanelComponent, PanelRegistry } from '@/api/types'
import { FallbackPanel } from './FallbackPanel'

// Lazy: a kind being registered does not mean the user opened it, so its chunk
// is downloaded on first use.
const VmsPanel = lazy(() => import('./vms/VmsPanel'))
const FilesPanel = lazy(() => import('./files/FilesPanel'))
const ConsolePanel = lazy(() => import('./console/ConsolePanel'))
const ShellPanel = lazy(() => import('./shell/ShellPanel'))

const renderers: Record<string, PanelComponent> = {
  vms: VmsPanel, // Guest registry, configuration pool, lifecycle actions.
  files: FilesPanel, // Staged file transfers and where they stand.
  console: ConsolePanel, // One terminal per guest console lane.
  shell: ShellPanel, // The hypervisor's own management shell.
}

export function resolvePanel(kind: string): PanelComponent {
  return renderers[kind] ?? FallbackPanel
}

export const panelRegistry: PanelRegistry = {
  resolve: resolvePanel,
  // Clicking a guest in the navigation opens its terminal.
  terminalKind: 'console',
}
