//! The file-transfer state machine: one implementation, several consumers.
//!
//! The file panel drives it with a drop target; the creation form drives it to
//! put a file where a config names it. Neither panel reaches into the other —
//! the composition root (`shell/App.tsx`) builds this service from the `files`
//! resource's declared operations and hands it down, so the transfer protocol
//! lives in exactly one place.
//!
//! The operations are the ones the hypervisor already declares, one per step:
//!
//! - **open** starts a session from a client-chosen id, so sending the same file
//!   twice resumes the first attempt instead of staging the bytes twice;
//! - **send** carries one chunk per request, so what a transfer holds in memory
//!   is bounded by the chunk size rather than by the file;
//! - **resume** (`HEAD`) is how an interrupted transfer asks where it stopped.
//!   The answer is the length of the staged file on disk, which is the only
//!   offset a client may continue from;
//! - **place** moves the staged bytes to their final name, and is the step that
//!   can be refused because that name is taken;
//! - **drop** forgets a session, and is refused once the file is placed: those
//!   bytes are what a config may already reference;
//! - **mkdir** is the one directory the interface creates, because a transfer
//!   target has to exist before bytes can go into it.
//!
//! Nothing here spells a request path: every URL comes from the injected link.

import { useSyncExternalStore } from 'react'
import type { ApiClient } from '@/api/client'
import {
  ApiError,
  describeError,
  type FileSession,
  type FileState,
  type FilesCapability,
  type FilesInfo,
  type FilesSnapshot,
  type PanelLink,
  type Transfer,
} from '@/api/types'

/**
 * Bytes per chunk request.
 *
 * The hypervisor refuses a chunk larger than its own limit and does not declare
 * that limit in the manifest, so this stays at or below it; the file is never
 * read whole, which is what keeps a large transfer off the heap.
 */
export const CHUNK_BYTES = 1024 * 1024

/** Longest session id the control plane accepts. */
const SESSION_ID_MAX = 64

/** Response header that reports how many bytes are on disk. */
const UPLOAD_OFFSET = 'Upload-Offset'

/** One rendered row, from the backend's listing or from a transfer in flight. */
export interface Row {
  id: string
  name: string
  directory: string
  written: number
  total: number
  state: FileState
  detail: string | null
  /** Present while this row is a transfer this service is driving. */
  transfer: Transfer | null
}

const EMPTY_SNAPSHOT: FilesSnapshot = { sessions: [], transfers: [] }

/**
 * The transfers and the session listing, as one observable value.
 *
 * A consumer subscribes and reads: the service is the store, so a transfer
 * started anywhere (the file panel, a creation form) shows up in every consumer
 * without either of them knowing about the other.
 */
export class FilesService implements FilesCapability {
  private sessions: FileSession[] = []
  private transfers: Transfer[] = []
  private snapshot: FilesSnapshot = EMPTY_SNAPSHOT
  private readonly listeners = new Set<() => void>()
  private readonly aborts = new Map<string, AbortController>()

  constructor(
    private readonly api: ApiClient,
    private readonly link: PanelLink,
  ) {}

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener)
    return () => {
      this.listeners.delete(listener)
    }
  }

  /** Stable between notifications, which is what `useSyncExternalStore` needs. */
  getSnapshot = (): FilesSnapshot => this.snapshot

  private publish(): void {
    this.snapshot = { sessions: this.sessions, transfers: this.transfers }
    for (const listener of this.listeners) listener()
  }

  private track(row: Transfer): void {
    const known = this.transfers.find((candidate) => candidate.id === row.id)
    this.transfers = known
      ? this.transfers.map((candidate) =>
          candidate.id === row.id ? { ...candidate, ...row } : candidate,
        )
      : [...this.transfers, row]
    this.publish()
  }

  private update(id: string, patch: Partial<Transfer>): void {
    this.transfers = this.transfers.map((row) => (row.id === id ? { ...row, ...patch } : row))
    this.publish()
  }

  /** Reads the backend's session listing. */
  async refresh(): Promise<void> {
    const info = await this.api.get<FilesInfo>(this.link.url('list'))
    this.sessions = info.files
    this.publish()
  }

  /**
   * Sends one file into `directory`, finally named `name`.
   *
   * `name` is the final name rather than always the file's own: a creation form
   * transfers to the exact path its config names, which may differ from the name
   * the operator picked the file under. A refusal is recorded on the transfer
   * instead of thrown, because a drop and a form submission both start one
   * without waiting for it.
   */
  async upload(file: File, directory: string, name: string = file.name): Promise<void> {
    const id = sessionId(file)
    this.aborts.set(id, new AbortController())
    this.track({
      id,
      name,
      directory,
      written: 0,
      total: file.size,
      phase: 'opening',
      detail: null,
      file,
    })
    try {
      // `open` is idempotent: sending the same file again answers with the
      // session it already has and the bytes already on disk, so a fresh send
      // and a restart share this path.
      const session = await this.api.post<FileSession>(this.link.url('open'), {
        id,
        directory,
        total: file.size,
      })
      await this.sendFrom(file, id, session.written, name)
    } catch (e: unknown) {
      if (e instanceof DOMException && e.name === 'AbortError') {
        this.update(id, { phase: 'failed', detail: '已取消' })
        return
      }
      this.update(id, { phase: 'failed', detail: describeError(e) })
    } finally {
      this.aborts.delete(id)
    }
  }

  /** Continues a transfer whose bytes this service still holds. */
  async resume(id: string): Promise<void> {
    const transfer = this.transfers.find((row) => row.id === id)
    const file = transfer?.file ?? null
    if (transfer === undefined || file === null) return
    this.aborts.set(id, new AbortController())
    try {
      // The offset comes from the disk, through the plane that owns it: a local
      // counter could be ahead of what survived the interruption.
      const offset = offsetOf(await this.api.head(this.link.url('resume', { id })))
      await this.sendFrom(file, id, offset, transfer.name)
    } catch (e: unknown) {
      this.update(id, { phase: 'failed', detail: describeError(e) })
    } finally {
      this.aborts.delete(id)
    }
  }

  /** Sends one file from `offset` to the end, then places it as `name`. */
  private async sendFrom(file: File, id: string, offset: number, name: string): Promise<void> {
    const signal = this.aborts.get(id)?.signal
    let at = offset
    this.update(id, { phase: 'sending', written: at })
    while (at < file.size) {
      const end = Math.min(at + CHUNK_BYTES, file.size)
      const headers = await this.api.patch(
        this.link.url('send', { id }),
        file.slice(at, end),
        {
          'Content-Type': 'application/octet-stream',
          'Content-Range': `bytes ${at}-${end - 1}/${file.size}`,
        },
        signal,
      )
      at = offsetOf(headers)
      this.update(id, { written: at })
    }
    await this.place(id, name)
  }

  /**
   * Moves finished bytes to their final name.
   *
   * A taken name is the one refusal that is about the request rather than the
   * transfer: the staged bytes are complete and stay complete, so the row asks
   * for another name instead of offering a resume. The refusal is recorded and
   * not thrown, so the caller that started a transfer does not have to catch it.
   */
  async place(id: string, name: string): Promise<boolean> {
    this.update(id, { phase: 'placing', detail: null })
    try {
      const placed = await this.api.post<FileSession>(this.link.url('place', { id }), { name })
      this.update(id, { phase: 'placed', name, written: placed.written, detail: null, file: null })
      await this.refresh()
      return true
    } catch (e: unknown) {
      const taken = e instanceof ApiError && e.status === 409
      this.update(id, { phase: taken ? 'needs-name' : 'failed', detail: describeError(e) })
      return false
    }
  }

  /** Forgets a session and the bytes it staged. */
  async drop(id: string): Promise<void> {
    // A transfer still running has to stop before its session goes away, or the
    // next chunk would arrive for a session that is gone.
    this.aborts.get(id)?.abort()
    await this.api.del(this.link.url('drop', { id }))
    this.transfers = this.transfers.filter((row) => row.id !== id)
    this.publish()
    await this.refresh()
  }

  /** Creates one directory level, returning the path it created. */
  async mkdir(parent: string, name: string): Promise<string> {
    const made = await this.api.post<{ path: string }>(this.link.url('mkdir'), {
      parent: parent.trim() || '/',
      name: name.trim(),
    })
    return made.path
  }
}

const NO_SUBSCRIPTION = (): (() => void) => () => {}
const NO_SNAPSHOT = (): FilesSnapshot => EMPTY_SNAPSHOT

/** Observes a service, or an empty value when this build has no transfer panel. */
export function useFiles(service: FilesCapability | null): FilesSnapshot {
  return useSyncExternalStore(
    service?.subscribe ?? NO_SUBSCRIPTION,
    service?.getSnapshot ?? NO_SNAPSHOT,
  )
}

/**
 * The rows to show: the backend's listing, plus the transfers in flight.
 *
 * A transfer in flight is not in the listing at all (the control plane does not
 * publish `uploading`), and a transfer that just ended is in both. So a local
 * row wins while it is still moving, and the listing wins once it has settled:
 * that way a row never appears twice and never shows a stale phase.
 */
export function mergeRows(sessions: FileSession[], transfers: Transfer[]): Row[] {
  const byId = new Map<string, Row>()
  for (const session of sessions) {
    byId.set(session.id, {
      id: session.id,
      name: session.name ?? '',
      directory: session.directory,
      written: session.written,
      total: session.total,
      state: session.state,
      detail: session.detail,
      transfer: null,
    })
  }
  for (const transfer of transfers) {
    const moving = isMoving(transfer)
    if (!moving && byId.has(transfer.id)) continue
    byId.set(transfer.id, {
      id: transfer.id,
      name: transfer.name,
      directory: transfer.directory,
      written: transfer.written,
      total: transfer.total,
      state: stateOf(transfer),
      detail: transfer.detail,
      transfer,
    })
  }
  return [...byId.values()]
}

/** Whether a transfer is still in the part of its life the listing cannot show. */
export function isMoving(transfer: Transfer): boolean {
  return (
    transfer.phase === 'opening' ||
    transfer.phase === 'sending' ||
    transfer.phase === 'placing' ||
    transfer.phase === 'needs-name'
  )
}

/** The reported state a locally tracked transfer corresponds to. */
export function stateOf(transfer: Transfer): FileState {
  switch (transfer.phase) {
    case 'opening':
    case 'sending':
      return 'uploading'
    case 'placing':
      return 'placing'
    case 'placed':
      return 'placed'
    case 'needs-name':
      return 'uploaded'
    case 'failed':
      return 'failed'
  }
}

/**
 * The id one file is staged under.
 *
 * Stable for one file, so sending it again resumes the first attempt, and
 * distinct for a different file that happens to share its name and length: the
 * modification stamp is part of it. The control plane accepts ascii
 * alphanumerics plus `-_.+` as a single path component.
 */
export function sessionId(file: File): string {
  const stem = file.name
    .replace(/[^A-Za-z0-9-_.+]/g, '-')
    // Two dots anywhere are refused by the control plane, and a leading one too,
    // because the id becomes one path component of the staging file. A single
    // dot is kept so the id still reads like the file it belongs to.
    .replace(/\.{2,}/g, '-')
    .replace(/^\.+/, '-')
    .slice(0, 32)
  return `${stem}-${file.size}-${file.lastModified}`.slice(0, SESSION_ID_MAX)
}

/** Reads the reported offset, which is the only position a client may trust. */
export function offsetOf(headers: Headers): number {
  const raw = headers.get(UPLOAD_OFFSET)
  const offset = raw === null ? Number.NaN : Number(raw)
  if (!Number.isFinite(offset)) {
    throw new Error('后端没有报告 Upload-Offset，无法确认已落盘长度')
  }
  return offset
}
