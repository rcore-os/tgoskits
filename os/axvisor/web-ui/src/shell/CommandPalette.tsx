//! Command palette: one keystroke to anywhere the navigation can reach.
//!
//! Everything it offers is something the manifest already declared — a panel
//! to open, a guest to open a terminal for — so the palette adds no capability
//! of its own and cannot offer an operation this build does not serve. It is
//! the same two lists the navigation shows, reachable without moving a hand to
//! the mouse, which is the difference on a keyboard-driven console.

import { useEffect, useMemo, useRef, useState } from 'react'
import { CornerDownLeft, Search } from 'lucide-react'
import { describeStatus, type PanelMeta, type VmSummary } from '@/api/types'
import { cn } from '@/lib/utils'

export interface CommandPaletteProps {
  open: boolean
  panels: PanelMeta[]
  resources: VmSummary[]
  onOpen: (kind: string) => void
  onOpenVm: (id: number) => void
  onClose: () => void
}

interface Entry {
  /** Stable key: a panel kind and a guest id can otherwise collide. */
  key: string
  label: string
  hint: string
  run: () => void
}

const MAX_ENTRIES = 40

export function CommandPalette({
  open,
  panels,
  resources,
  onOpen,
  onOpenVm,
  onClose,
}: CommandPaletteProps) {
  const [query, setQuery] = useState('')
  const [cursor, setCursor] = useState(0)
  const inputRef = useRef<HTMLInputElement>(null)

  // A reopened palette starts empty: the operator is looking for something
  // new, and the previous query is not what they meant to keep.
  useEffect(() => {
    if (!open) return
    setQuery('')
    setCursor(0)
    inputRef.current?.focus()
  }, [open])

  const entries = useMemo<Entry[]>(() => {
    const needle = query.trim().toLowerCase()
    const all: Entry[] = [
      ...panels.map((panel) => ({
        key: `panel:${panel.kind}`,
        label: panel.title,
        hint: panel.kind,
        run: () => onOpen(panel.kind),
      })),
      ...resources.map((vm) => ({
        key: `vm:${vm.id}`,
        label: `VM[${vm.id}] ${vm.name}`.trim(),
        hint: describeStatus(vm.status),
        run: () => onOpenVm(vm.id),
      })),
    ]
    const matched =
      needle.length === 0
        ? all
        : all.filter(
            (entry) =>
              entry.label.toLowerCase().includes(needle) ||
              entry.hint.toLowerCase().includes(needle),
          )
    // The list is capped because the palette is a way to reach a known target,
    // not a search result page: forty rows is more than a screen already.
    return matched.slice(0, MAX_ENTRIES)
  }, [panels, resources, query, onOpen, onOpenVm])

  if (!open) return null

  const choose = (entry: Entry | undefined) => {
    if (!entry) return
    entry.run()
    onClose()
  }

  return (
    <div
      className="fixed inset-0 z-50 flex items-start justify-center bg-black/50 pt-[12vh] backdrop-blur-sm"
      onClick={onClose}
    >
      <div
        className="w-full max-w-lg overflow-hidden rounded-lg border bg-popover shadow-2xl"
        onClick={(event) => event.stopPropagation()}
      >
        <div className="flex items-center gap-2 border-b px-3">
          <Search className="h-4 w-4 shrink-0 text-muted-foreground" />
          <input
            ref={inputRef}
            value={query}
            onChange={(event) => {
              setQuery(event.target.value)
              setCursor(0)
            }}
            onKeyDown={(event) => {
              if (event.key === 'Escape') {
                onClose()
              } else if (event.key === 'ArrowDown') {
                event.preventDefault()
                setCursor((index) => Math.min(index + 1, entries.length - 1))
              } else if (event.key === 'ArrowUp') {
                event.preventDefault()
                setCursor((index) => Math.max(index - 1, 0))
              } else if (event.key === 'Enter') {
                event.preventDefault()
                choose(entries[cursor])
              }
            }}
            placeholder="打开面板，或打开客户机终端…"
            className="h-11 w-full bg-transparent text-sm outline-none placeholder:text-muted-foreground"
          />
        </div>
        <ul className="max-h-72 overflow-y-auto p-1">
          {entries.map((entry, index) => (
            <li key={entry.key}>
              <button
                type="button"
                onClick={() => choose(entry)}
                onMouseEnter={() => setCursor(index)}
                className={cn(
                  'flex w-full items-center justify-between gap-3 rounded-md px-2.5 py-2 text-left text-sm',
                  index === cursor ? 'bg-accent text-accent-foreground' : 'text-foreground',
                )}
              >
                <span className="truncate">{entry.label}</span>
                <span className="flex items-center gap-2 font-mono text-xs text-muted-foreground">
                  {entry.hint}
                  {index === cursor && <CornerDownLeft className="h-3 w-3" />}
                </span>
              </button>
            </li>
          ))}
          {entries.length === 0 && (
            <li className="px-3 py-6 text-center text-sm text-muted-foreground">
              没有匹配的面板或客户机
            </li>
          )}
        </ul>
      </div>
    </div>
  )
}
