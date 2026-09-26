//! A picker for a file that is already in the guest filesystem — and for making
//! the folder it goes into.
//!
//! The candidates are the listing's files, which is what makes a picked path a
//! placed one by construction: nothing is offered that the folder read did not
//! see. The picker owns no path of its own — every listing comes from the
//! `browse` operation the caller passes in, and every folder it creates goes
//! through the caller's `mkdir` — and it walks directories the way the folder
//! views do.
//!
//! Folders can be made from a right-click, the way a file manager does it: on a
//! folder to make one *inside* it, or on the empty space to make one in the
//! folder being shown. The new folder is entered, so it is obvious it exists.

import { useCallback, useEffect, useState, type MouseEvent } from 'react'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import type { ApiClient } from '@/api/client'
import { describeError, type BrowseInfo } from '@/api/types'

/** What a context menu was opened on: the folder to act in, or the file to take. */
interface Menu {
  x: number
  y: number
  kind: 'directory' | 'blank' | 'file'
  parent: string
  file?: string
}

export function FilePicker({
  api,
  listing,
  makeFolder,
  open,
  onOpenChange,
  onPick,
  initial,
}: {
  api: ApiClient
  /** URL of one listing, built by the caller from its own declared operation. */
  listing(path: string): string
  /** Creates one folder level, returning the path that was made. */
  makeFolder(parent: string, name: string): Promise<string>
  open: boolean
  onOpenChange(open: boolean): void
  onPick(path: string): void
  /** Folder the picker opens at, and stays on until one is picked. */
  initial: string
}) {
  const [path, setPath] = useState(initial)
  const [folder, setFolder] = useState<BrowseInfo | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [menu, setMenu] = useState<Menu | null>(null)
  // The folder a name is being asked for, while the inline input is open.
  const [naming, setNaming] = useState<string | null>(null)
  const [name, setName] = useState('')

  // The listing is read for the folder being shown, so entering a folder the
  // form never anticipated costs one request and nothing else.
  useEffect(() => {
    if (!open) return
    let cancelled = false
    void (async () => {
      try {
        const shown = await api.get<BrowseInfo>(listing(path))
        if (cancelled) return
        setFolder(shown)
        setError(null)
      } catch (e: unknown) {
        if (cancelled) return
        setFolder(null)
        setError(describeError(e))
      }
    })()
    return () => {
      cancelled = true
    }
  }, [api, listing, open, path])

  // Opening again starts where the caller says, not where the last walk ended,
  // and a menu or name input from a closed dialog must not survive it.
  useEffect(() => {
    if (!open) return
    setPath(initial)
    setMenu(null)
    setNaming(null)
  }, [initial, open])

  const enter = useCallback((next: string) => {
    setPath(next)
    setMenu(null)
  }, [])

  /** Creates the named folder inside `parent` and walks into it. */
  const create = useCallback(
    async (parent: string, wanted: string) => {
      const trimmed = wanted.trim()
      if (trimmed.length === 0) return
      try {
        const made = await makeFolder(parent, trimmed)
        setNaming(null)
        setName('')
        setError(null)
        setPath(made)
      } catch (e: unknown) {
        setError(describeError(e))
      }
    },
    [makeFolder],
  )

  const listMenu = useCallback((event: MouseEvent, opened: Omit<Menu, 'x' | 'y'>) => {
    event.preventDefault()
    setMenu({ ...opened, x: event.clientX, y: event.clientY })
  }, [])

  const take = useCallback(
    (picked: string) => {
      onPick(picked)
      onOpenChange(false)
    },
    [onOpenChange, onPick],
  )

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>选择已就位的文件</DialogTitle>
          <DialogDescription>
            这里列出的是客户机文件系统里已经在的文件，点一个填进表单；在文件夹上或空白处右键可以就地新建一层。
            还没传上去的文件不会出现——先传到位，再回来选。
          </DialogDescription>
        </DialogHeader>

        <div className="flex flex-wrap items-center gap-2">
          <code className="flex-1 truncate rounded border bg-muted px-2 py-1 text-xs">{path}</code>
          <Button
            size="sm"
            variant="outline"
            disabled={folder?.parent == null}
            onClick={() => {
              if (folder?.parent) enter(folder.parent)
            }}
          >
            上一级
          </Button>
          <Button
            size="sm"
            variant="outline"
            onClick={() => {
              setName('')
              setNaming(path)
            }}
          >
            新建文件夹
          </Button>
        </div>

        {/* The empty space of the list is the folder being shown, so a
            right-click here makes a folder in it. */}
        <div
          className="max-h-[45vh] overflow-y-auto"
          onContextMenu={(event) =>
            listMenu(event, { kind: 'blank', parent: path })
          }
        >
          <ul className="flex flex-col gap-1">
            {naming !== null && naming === path && (
              <li className="flex items-center gap-2">
                <Input
                  autoFocus
                  className="h-8 font-mono text-xs"
                  value={name}
                  placeholder="新目录名"
                  onChange={(event) => setName(event.target.value)}
                  onKeyDown={(event) => {
                    if (event.key === 'Enter') void create(naming, name)
                    if (event.key === 'Escape') setNaming(null)
                  }}
                />
                <Button size="sm" onClick={() => void create(naming, name)}>
                  建立
                </Button>
              </li>
            )}
            {(folder?.directories ?? []).map((directory) => (
              <li key={directory.path}>
                {naming === directory.path ? (
                  <div className="flex items-center gap-2">
                    <Input
                      autoFocus
                      className="h-8 font-mono text-xs"
                      value={name}
                      placeholder="新目录名"
                      onChange={(event) => setName(event.target.value)}
                      onKeyDown={(event) => {
                        if (event.key === 'Enter') void create(directory.path, name)
                        if (event.key === 'Escape') setNaming(null)
                      }}
                    />
                    <Button size="sm" onClick={() => void create(directory.path, name)}>
                      建立
                    </Button>
                  </div>
                ) : (
                  <button
                    type="button"
                    className="w-full rounded px-2 py-1 text-left font-mono text-xs hover:bg-muted"
                    onClick={() => enter(directory.path)}
                    onContextMenu={(event) =>
                      listMenu(event, { kind: 'directory', parent: directory.path })
                    }
                  >
                    [dir] {directory.name}
                  </button>
                )}
              </li>
            ))}
            {(folder?.files ?? []).map((entry) => (
              <li key={entry.path}>
                <button
                  type="button"
                  className="flex w-full items-center justify-between rounded px-2 py-1 text-left font-mono text-xs hover:bg-muted"
                  onClick={() => take(entry.path)}
                  onContextMenu={(event) =>
                    listMenu(event, { kind: 'file', parent: path, file: entry.path })
                  }
                >
                  <span className="truncate">{entry.name}</span>
                  <span className="text-muted-foreground">{entry.size} B</span>
                </button>
              </li>
            ))}
            {folder !== null &&
              folder.directories.length === 0 &&
              folder.files.length === 0 &&
              naming === null && (
                <li className="py-2 text-center text-sm text-muted-foreground">
                  这个目录里没有文件，也没有子目录——右键新建一层，或把文件拖到表单上。
                </li>
              )}
          </ul>
        </div>

        {(folder?.issues ?? []).map((issue) => (
          <p key={issue.path} className="text-xs text-muted-foreground">
            读不了 {issue.path}：{issue.detail}
          </p>
        ))}
        {error && (
          <p className="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm">
            {error}
          </p>
        )}

        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            取消
          </Button>
        </DialogFooter>

        {menu && (
          <>
            {/* Clicking anywhere else dismisses the menu, the way a context
                menu behaves: the click does not also act on what is under it. */}
            <div
              className="fixed inset-0 z-40"
              onClick={() => setMenu(null)}
              onContextMenu={(event) => {
                event.preventDefault()
                setMenu(null)
              }}
            />
            <div
              className="fixed z-50 flex min-w-44 flex-col rounded-md border bg-background p-1 text-sm shadow-md"
              style={{ left: menu.x, top: menu.y }}
            >
              {menu.kind !== 'file' && (
                <button
                  type="button"
                  className="rounded px-2 py-1 text-left hover:bg-muted"
                  onClick={() => {
                    setName('')
                    setNaming(menu.parent)
                    setMenu(null)
                  }}
                >
                  在此新建文件夹
                </button>
              )}
              {menu.kind === 'directory' && (
                <button
                  type="button"
                  className="rounded px-2 py-1 text-left hover:bg-muted"
                  onClick={() => enter(menu.parent)}
                >
                  进入此目录
                </button>
              )}
              {menu.kind === 'file' && menu.file !== undefined && (
                <button
                  type="button"
                  className="rounded px-2 py-1 text-left hover:bg-muted"
                  onClick={() => take(menu.file as string)}
                >
                  选择此文件
                </button>
              )}
            </div>
          </>
        )}
      </DialogContent>
    </Dialog>
  )
}
