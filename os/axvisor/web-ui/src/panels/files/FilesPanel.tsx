//! File panel: drops arbitrary files into the guest filesystem.
//!
//! The panel owns no transfer policy of its own: it renders the machine in
//! `domain/files.ts`, which the composition root hands to every panel that
//! needs it. What is left here is the view — the folder you are looking at, the
//! drop target it is, and how a transfer's state reads on screen.
//!
//! The listing is the drop target: the folder you are looking at is where a
//! dropped file goes, the way a file manager works. A transfer's progress is the
//! offset the backend reported last, so a row cannot claim more than what is on
//! disk.

import { useCallback, useEffect, useRef, useState, type DragEvent, type ReactNode } from 'react'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import {
  describeError,
  type FileState,
  type FolderListing,
  type PanelProps,
} from '@/api/types'
import { mergeRows, useFiles } from '@/domain/files'
import { cn } from '@/lib/utils'
import { FolderPicker } from './FolderPicker'

export default function FilesPanel({ api, link, files }: PanelProps) {
  const snapshot = useFiles(files ?? null)
  const [target, setTarget] = useState('/')
  const [viewPath, setViewPath] = useState('/')
  const [view, setView] = useState<FolderListing | null>(null)
  const [viewError, setViewError] = useState<string | null>(null)
  const [pickOpen, setPickOpen] = useState(false)
  const [naming, setNaming] = useState(false)
  const [newName, setNewName] = useState('')
  const [names, setNames] = useState<Record<string, string>>({})
  const [busy, setBusy] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [note, setNote] = useState<string | null>(null)
  const chooser = useRef<HTMLInputElement | null>(null)
  const placed = useRef(new Set<string>())

  if (files == null) {
    // The shell builds this service because the manifest declares this panel, so
    // a panel without one is a wiring bug; it has to be visible rather than
    // turning into a panel whose buttons do nothing.
    throw new Error('本构建没有声明文件传输能力，文件面板没有可驱动的动作')
  }
  const service = files

  // The browse link has no placeholder — the folder is a query parameter — so
  // the panel builds the one request target the folder view asks for. Stable
  // across renders, or the view would re-read the folder on every one of them.
  const listingUrl = useCallback(
    (path: string) => `${link.url('browse')}?path=${encodeURIComponent(path)}`,
    [link],
  )

  /** Reads one folder of the guest filesystem into the listing. */
  const loadView = useCallback(
    async (path: string) => {
      try {
        const shown = await api.get<FolderListing>(listingUrl(path))
        setView(shown)
        setViewError(null)
      } catch (e: unknown) {
        setView(null)
        setViewError(describeError(e))
      }
    },
    [api, listingUrl],
  )

  useEffect(() => {
    void loadView(viewPath)
  }, [loadView, viewPath])

  // A transfer that has just landed changes what this folder holds, wherever it
  // was started. Watching the service is how the view follows it without either
  // side knowing about the other.
  useEffect(() => {
    let landed = false
    for (const transfer of snapshot.transfers) {
      if (transfer.phase === 'placed' && !placed.current.has(transfer.id)) {
        placed.current.add(transfer.id)
        landed = true
      }
    }
    if (landed) void loadView(viewPath)
  }, [snapshot.transfers, loadView, viewPath])

  const refreshSessions = useCallback(async () => {
    try {
      await service.refresh()
      setError(null)
    } catch (e: unknown) {
      setError(describeError(e))
    }
  }, [service])

  useEffect(() => {
    void refreshSessions()
  }, [refreshSessions])

  const put = useCallback(
    (file: File, directory: string) => {
      void service.upload(file, directory)
    },
    [service],
  )

  const dropped = useCallback(
    (event: DragEvent<HTMLDivElement>) => {
      event.preventDefault()
      for (const file of Array.from(event.dataTransfer.files)) {
        put(file, viewPath)
      }
    },
    [put, viewPath],
  )

  const retryPlace = useCallback(
    async (id: string, name: string) => {
      setBusy(id)
      try {
        const ok = await service.place(id, name)
        if (ok) {
          setNote(`「${name}」已就位`)
          setError(null)
          await loadView(viewPath)
        }
      } catch (e: unknown) {
        setError(describeError(e))
      } finally {
        setBusy(null)
      }
    },
    [loadView, service, viewPath],
  )

  const resume = useCallback(
    async (id: string) => {
      setBusy(id)
      try {
        await service.resume(id)
      } finally {
        setBusy(null)
      }
    },
    [service],
  )

  const drop = useCallback(
    async (id: string) => {
      setBusy(id)
      try {
        await service.drop(id)
        setNote(`已丢弃 ${id}`)
        setError(null)
        await loadView(viewPath)
      } catch (e: unknown) {
        setError(describeError(e))
      } finally {
        setBusy(null)
      }
    },
    [loadView, service, viewPath],
  )

  /** Creates one folder level in the folder on screen, and walks into it. */
  const makeFolder = useCallback(async () => {
    const name = newName.trim()
    if (name.length === 0) return
    setBusy('folder')
    try {
      const made = await service.mkdir(viewPath, name)
      setNaming(false)
      setNewName('')
      setTarget(made)
      setViewPath(made)
      setNote(`已新建目录 ${made}`)
      setError(null)
    } catch (e: unknown) {
      setError(describeError(e))
    } finally {
      setBusy(null)
    }
  }, [newName, service, viewPath])

  const createFolder = useCallback(
    (parent: string, name: string) => service.mkdir(parent, name),
    [service],
  )

  const rows = mergeRows(snapshot.sessions, snapshot.transfers)

  return (
    <div className="flex flex-col gap-3 p-4">
      <Card>
        <CardHeader className="flex-row items-center justify-between space-y-0">
          <div>
            <CardTitle>文件</CardTitle>
            <CardDescription>
              一个文件夹视图：拖入或选择文件就传到当前目录，再放置到最终名。已落盘长度以磁盘为准，
              中断后重新拖入同一文件即可续传；已经就位的文件不能再被丢弃。
            </CardDescription>
          </div>
          <Button size="sm" variant="outline" onClick={() => void refreshSessions()}>
            刷新清单
          </Button>
        </CardHeader>
        <CardContent className="flex flex-col gap-3 text-sm">
          <div className="flex flex-wrap items-center gap-2">
            <Input
              className="h-8 w-72 font-mono text-xs"
              value={target}
              spellCheck={false}
              placeholder="/"
              onChange={(event) => setTarget(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === 'Enter') {
                  const path = target.trim() || '/'
                  setTarget(path)
                  setViewPath(path)
                }
              }}
            />
            <Button
              size="sm"
              variant="outline"
              disabled={view?.parent == null}
              onClick={() => {
                if (view?.parent) {
                  setTarget(view.parent)
                  setViewPath(view.parent)
                }
              }}
            >
              上一级
            </Button>
            <Button size="sm" variant="outline" onClick={() => void loadView(viewPath)}>
              刷新
            </Button>
            <Button
              size="sm"
              variant="outline"
              onClick={() => {
                setNewName('')
                setNaming(true)
              }}
            >
              新建文件夹
            </Button>
            <Button size="sm" variant="outline" onClick={() => setPickOpen(true)}>
              选择目录…
            </Button>
          </div>

          {naming && (
            <div className="flex items-center gap-2">
              <Input
                autoFocus
                className="h-8 w-56 font-mono text-xs"
                value={newName}
                placeholder="新目录名"
                onChange={(event) => setNewName(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === 'Enter') void makeFolder()
                  if (event.key === 'Escape') setNaming(false)
                }}
              />
              <Button size="sm" disabled={busy !== null} onClick={() => void makeFolder()}>
                建立
              </Button>
              <Button size="sm" variant="outline" onClick={() => setNaming(false)}>
                取消
              </Button>
            </div>
          )}

          {/* The listing is the drop target: the folder you are looking at is
              where a dropped file goes, the way a file manager works. */}
          <div
            className="flex max-h-[40vh] flex-col gap-1 overflow-y-auto rounded-md border border-dashed p-2"
            onDragOver={(event) => event.preventDefault()}
            onDrop={dropped}
          >
            {(view?.directories ?? []).map((directory) => (
              <button
                key={directory.path}
                type="button"
                className="rounded px-2 py-1 text-left font-mono text-xs hover:bg-muted"
                onClick={() => {
                  setTarget(directory.path)
                  setViewPath(directory.path)
                }}
              >
                [dir] {directory.name}
              </button>
            ))}
            {(view?.files ?? []).map((file) => (
              <div
                key={file.path}
                className="flex items-center justify-between px-2 py-1 font-mono text-xs"
              >
                <span className="truncate">{file.name}</span>
                <span className="text-muted-foreground">{file.size} B</span>
              </div>
            ))}
            {view !== null && view.directories.length === 0 && view.files.length === 0 && (
              <p className="py-3 text-center text-muted-foreground">
                这个目录是空的——把文件拖进来就会传到 <span className="font-mono">{view.path}</span>。
              </p>
            )}
            {viewError && <p className="py-3 text-center text-muted-foreground">{viewError}</p>}
          </div>

          {(view?.issues ?? []).map((issue) => (
            <p key={issue.path} className="text-xs text-muted-foreground">
              读不了 {issue.path}：{issue.detail}
            </p>
          ))}

          <div className="flex flex-wrap items-center gap-2">
            <Button size="sm" onClick={() => chooser.current?.click()}>
              选择文件…
            </Button>
            <span className="text-muted-foreground">
              或把文件拖进上面的列表：落到 <span className="font-mono">{viewPath}</span>
            </span>
          </div>
          <input
            ref={chooser}
            type="file"
            multiple
            className="hidden"
            onChange={(event) => {
              for (const file of Array.from(event.target.files ?? [])) {
                put(file, viewPath)
              }
              // The same file may be chosen again after a failure, which would
              // not fire `change` if the value stayed set.
              event.target.value = ''
            }}
          />

          {error && <Banner tone="error">{error}</Banner>}
          {note && <Banner tone="info">{note}</Banner>}
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>传输</CardTitle>
          <CardDescription>
            来自 `GET /api/files`：正在传输的对象不在清单里，传完但未放置的（uploaded）与已就位的
            （placed）才是；`written` 是磁盘上的字节数，也就是可以续传的位置。
          </CardDescription>
        </CardHeader>
        <CardContent>
          <table className="w-full text-sm">
            <thead className="text-left text-xs uppercase text-muted-foreground">
              <tr>
                <th className="py-1">文件</th>
                <th className="py-1">状态</th>
                <th className="py-1">进度</th>
                <th className="py-1">目标目录</th>
                <th className="py-1 text-right">操作</th>
              </tr>
            </thead>
            <tbody>
              {rows.map((row) => {
                // A transfer this service drove and that failed still holds the
                // bytes, which is the only reason 继续 is possible at all.
                const transfer = row.transfer
                return (
                  <tr key={row.id} className="border-t align-top">
                    <td className="py-1">
                      <div className="flex flex-col">
                        <span>{row.name || row.id}</span>
                        <span className="font-mono text-xs text-muted-foreground">{row.id}</span>
                      </div>
                    </td>
                    <td className="py-1">
                      <StateBadge state={row.state} />
                      {row.detail && (
                        <p className="mt-1 max-w-64 text-xs text-muted-foreground">{row.detail}</p>
                      )}
                    </td>
                    <td className="w-44 py-1">
                      <Progress written={row.written} total={row.total} />
                    </td>
                    <td className="py-1 font-mono text-xs">{row.directory}</td>
                    <td className="flex flex-wrap justify-end gap-1 py-1">
                      {row.state === 'uploaded' && (
                        <>
                          <Input
                            className="h-8 w-40"
                            value={names[row.id] ?? transfer?.name ?? ''}
                            placeholder="最终文件名"
                            onChange={(event) =>
                              setNames((current) => ({ ...current, [row.id]: event.target.value }))
                            }
                          />
                          <Button
                            size="sm"
                            disabled={busy !== null}
                            onClick={() =>
                              void retryPlace(
                                row.id,
                                (names[row.id] ?? transfer?.name ?? '').trim(),
                              )
                            }
                          >
                            放置
                          </Button>
                        </>
                      )}
                      {transfer?.phase === 'failed' && transfer.file && (
                        <Button
                          size="sm"
                          variant="outline"
                          disabled={busy !== null}
                          onClick={() => void resume(row.id)}
                        >
                          继续
                        </Button>
                      )}
                      {row.state !== 'placed' && (
                        <Button
                          size="sm"
                          variant="destructive"
                          disabled={busy !== null}
                          onClick={() => void drop(row.id)}
                        >
                          丢弃
                        </Button>
                      )}
                    </td>
                  </tr>
                )
              })}
              {rows.length === 0 && (
                <tr>
                  <td colSpan={5} className="py-3 text-center text-muted-foreground">
                    还没有传输。拖一个文件进来，它会先分块落到暂存区，再放置到目标目录。
                  </td>
                </tr>
              )}
            </tbody>
          </table>
        </CardContent>
      </Card>

      <FolderPicker
        api={api}
        listing={listingUrl}
        makeFolder={createFolder}
        open={pickOpen}
        onOpenChange={setPickOpen}
        onPick={(path) => {
          setTarget(path)
          setViewPath(path)
        }}
        initial={target}
      />
    </div>
  )
}

const FILE_STATE_TEXT: Record<FileState, string> = {
  uploading: '传输中',
  uploaded: '已传完·未就位',
  placing: '放置中',
  placed: '已就位',
  failed: '失败',
}

const FILE_STATE_TONE: Record<FileState, string> = {
  uploading: 'border-sky-300 bg-sky-50 text-sky-700',
  uploaded: 'border-amber-300 bg-amber-50 text-amber-700',
  placing: 'border-amber-300 bg-amber-50 text-amber-700',
  placed: 'border-emerald-300 bg-emerald-50 text-emerald-700',
  failed: 'border-red-300 bg-red-50 text-red-700',
}

function StateBadge({ state }: { state: FileState }) {
  return (
    <span className={cn('rounded-full border px-2 py-0.5 text-xs', FILE_STATE_TONE[state])}>
      {FILE_STATE_TEXT[state]}
    </span>
  )
}

function Progress({ written, total }: { written: number; total: number }) {
  const percent = total > 0 ? Math.min(100, Math.round((written / total) * 100)) : 0
  return (
    <div className="flex flex-col gap-1">
      <div className="h-1.5 w-full rounded bg-muted">
        <div className="h-1.5 rounded bg-primary" style={{ width: `${percent}%` }} />
      </div>
      <span className="font-mono text-xs text-muted-foreground">
        {written} / {total} B · {percent}%
      </span>
    </div>
  )
}

function Banner({ tone, children }: { tone: 'info' | 'error'; children: ReactNode }) {
  return (
    <p
      className={cn(
        'rounded-md border px-3 py-2 text-sm',
        tone === 'error' ? 'border-destructive/40 bg-destructive/10' : 'bg-muted',
      )}
    >
      {children}
    </p>
  )
}
