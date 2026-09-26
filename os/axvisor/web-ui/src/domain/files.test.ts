//! The transfer machine, driven without a browser.
//!
//! The engine used to live inside the file panel and had no test at all, so a
//! wrong frame — the `Content-Range` in particular — would only show up in a
//! QEMU run, if at all. These tests drive it with a recording fake, so the
//! request sequence itself is the assertion.
//!
//! The rest is the machine's model: which rows exist, and under which id a file
//! is staged. `mergeRows` decides what a listing holds (the backend's listing
//! and the live transfers are two sources for one row, and a row must not appear
//! twice or show a phase that is already over), and `sessionId` decides what a
//! second send of a file does — resume it or stage a second copy.

import { describe, expect, it } from 'vitest'
import type { ApiClient } from '@/api/client'
import { ApiError, type FileSession, type PanelLink, type Transfer } from '@/api/types'
import { CHUNK_BYTES, FilesService, mergeRows, offsetOf, sessionId } from './files'

function session(id: string, patch: Partial<FileSession> = {}): FileSession {
  return {
    id,
    directory: '/guest',
    name: null,
    path: null,
    total: 10,
    written: 10,
    state: 'uploaded',
    detail: null,
    ...patch,
  }
}

function transfer(id: string, patch: Partial<Transfer> = {}): Transfer {
  return {
    id,
    name: `${id}.img`,
    directory: '/guest',
    written: 0,
    total: 10,
    phase: 'sending',
    detail: null,
    file: null,
    ...patch,
  }
}

function file(name: string, size: number, lastModified = 1000): File {
  return new File([new Uint8Array(size)], name, { lastModified })
}

/** A link that only has to be distinguishable, never to be a real path. */
function panelLink(): PanelLink {
  return {
    url: (name, params) => `files/${name}/${JSON.stringify(params ?? {})}`,
    maybeUrl: (name) => `files/${name}`,
    declared: () => true,
  }
}

interface Recorded {
  method: string
  path: string
  body?: unknown
  range?: string
  bytes?: number
}

/**
 * A recording stand-in for the client.
 *
 * `ApiClient` is a class with private members, so a structural double is not
 * assignable and is cast once here. Only the methods the machine uses exist.
 */
class FakeApi {
  readonly calls: Recorded[] = []
  /** Bytes the next `HEAD` reports: where a resume continues from. */
  offset = 0
  /** Length the last `open` declared, which `place` reports back as written. */
  total = 0
  placeError: ApiError | null = null

  async get<T>(path: string): Promise<T> {
    this.calls.push({ method: 'GET', path })
    return { files: [] } as T
  }

  async post<T>(path: string, body?: unknown): Promise<T> {
    this.calls.push({ method: 'POST', path, body })
    if (path.includes('place')) {
      if (this.placeError !== null) throw this.placeError
      return session('placed', { name: 'placed', written: this.total }) as T
    }
    const declared = body as { total?: number } | undefined
    this.total = declared?.total ?? 0
    return session('uploading', { written: this.offset, total: this.total }) as T
  }

  async patch(path: string, body: Blob, headers: Record<string, string>): Promise<Headers> {
    this.calls.push({
      method: 'PATCH',
      path,
      range: headers['Content-Range'],
      bytes: body.size,
    })
    const end = Number(headers['Content-Range'].split('-')[1].split('/')[0])
    return new Headers({ 'Upload-Offset': String(end + 1) })
  }

  async head(path: string): Promise<Headers> {
    this.calls.push({ method: 'HEAD', path })
    return new Headers({ 'Upload-Offset': String(this.offset) })
  }

  async del(path: string): Promise<number> {
    this.calls.push({ method: 'DELETE', path })
    return 204
  }
}

function service(fake: FakeApi): FilesService {
  return new FilesService(fake as unknown as ApiClient, panelLink())
}

describe('mergeRows', () => {
  it('shows a transfer the listing does not have yet', () => {
    // An in-flight transfer is never listed: the control plane does not publish
    // `uploading`, so without this the row would only appear once it finished.
    const rows = mergeRows([], [transfer('kernel', { written: 4 })])
    expect(rows).toHaveLength(1)
    expect(rows[0]).toMatchObject({ id: 'kernel', state: 'uploading', written: 4 })
  })

  it('lets a moving transfer win over the listing row of the same id', () => {
    // The same id can be in both sources at once (a resumed transfer). The
    // listing's `uploaded` would read as "done" while bytes are still arriving.
    const rows = mergeRows([session('kernel')], [transfer('kernel', { phase: 'sending' })])
    expect(rows).toHaveLength(1)
    expect(rows[0].state).toBe('uploading')
    expect(rows[0].transfer).not.toBeNull()
  })

  it('lets the listing win once a transfer has settled', () => {
    // After place, the listing is the authority for the final name and path; a
    // local copy of "placed" would go stale on the next refresh.
    const rows = mergeRows(
      [session('kernel', { state: 'placed', name: 'kernel.img', path: '/guest/kernel.img' })],
      [transfer('kernel', { phase: 'placed' })],
    )
    expect(rows).toHaveLength(1)
    expect(rows[0]).toMatchObject({ state: 'placed', name: 'kernel.img', transfer: null })
  })

  it('keeps a settled transfer the listing has not picked up', () => {
    const rows = mergeRows([], [transfer('kernel', { phase: 'failed', detail: '连接中断' })])
    expect(rows).toHaveLength(1)
    expect(rows[0]).toMatchObject({ state: 'failed', detail: '连接中断' })
  })

  it('reports a transfer that is only waiting for a name as uploaded', () => {
    // `place` was refused because the name is taken: the bytes are complete and
    // staged, which is exactly the state the backend reports for it.
    const rows = mergeRows([], [transfer('kernel', { phase: 'needs-name', written: 10 })])
    expect(rows[0].state).toBe('uploaded')
  })
})

describe('sessionId', () => {
  it('is stable for one file, so sending it again resumes the same session', () => {
    expect(sessionId(file('linux-qemu', 3, 1000))).toBe(sessionId(file('linux-qemu', 3, 1000)))
  })

  it('differs for a different file that shares the name and length', () => {
    // Resuming a session whose staged bytes belong to another file would splice
    // two files together: the offset matches, but the content does not.
    expect(sessionId(file('linux-qemu', 3, 1000))).not.toBe(sessionId(file('linux-qemu', 3, 2000)))
    expect(sessionId(file('linux-qemu', 3, 1000))).not.toBe(sessionId(file('linux-qemu', 4, 1000)))
  })

  it('is one acceptable path component', () => {
    // The control plane turns the id into a file name: ascii alphanumerics plus
    // `-_.+`, no leading dot, at most 64 characters.
    const id = sessionId(file('../../etc/passwd ünïcode', 12, 1))
    expect(id.length).toBeLessThanOrEqual(64)
    expect(id).toMatch(/^[A-Za-z0-9-_.+]+$/)
    expect(id.startsWith('.')).toBe(false)
    expect(id.includes('..')).toBe(false)
    expect(id.includes('/')).toBe(false)
  })
})

describe('offsetOf', () => {
  it('reads the offset the backend reported', () => {
    expect(offsetOf(new Headers({ 'Upload-Offset': '736' }))).toBe(736)
  })

  it('refuses to guess when the answer carries no offset', () => {
    // A local count would be a different fact from what is on disk, and the
    // whole protocol is built on continuing from the disk.
    expect(() => offsetOf(new Headers())).toThrow()
    expect(() => offsetOf(new Headers({ 'Upload-Offset': 'unknown' }))).toThrow()
  })
})

describe('FilesService', () => {
  it('sends one chunk per request and places the file under the name it was given', async () => {
    const fake = new FakeApi()
    const transfers = service(fake)
    const size = CHUNK_BYTES + 1
    await transfers.upload(file('kernel.bin', size), '/guest/linux', 'linux-qemu')

    const sent = fake.calls.filter((call) => call.method === 'PATCH')
    expect(sent.map((call) => call.range)).toEqual([
      `bytes 0-${CHUNK_BYTES - 1}/${size}`,
      `bytes ${CHUNK_BYTES}-${size - 1}/${size}`,
    ])
    // The chunk is the frame's length, not the file's: nothing reads the whole
    // file into memory.
    expect(sent[0].bytes).toBe(CHUNK_BYTES)
    expect(sent[1].bytes).toBe(1)

    const placed = fake.calls.find((call) => call.path.includes('place'))
    expect(placed?.body).toEqual({ name: 'linux-qemu' })
    expect(transfers.getSnapshot().transfers[0]).toMatchObject({
      phase: 'placed',
      name: 'linux-qemu',
      written: size,
    })
  })

  it('continues from the offset the disk reports, not from a local count', async () => {
    const fake = new FakeApi()
    fake.offset = CHUNK_BYTES
    const transfers = service(fake)
    const size = CHUNK_BYTES + 1
    const kernel = file('kernel.bin', size)
    // A refusal at the last step keeps the bytes, which is the only situation a
    // resume exists for.
    fake.placeError = new ApiError(409, '目标已存在')
    await transfers.upload(kernel, '/guest/linux', 'linux-qemu')
    fake.calls.length = 0
    fake.placeError = null

    await transfers.resume(sessionId(kernel))

    expect(fake.calls[0]).toMatchObject({ method: 'HEAD' })
    const sent = fake.calls.filter((call) => call.method === 'PATCH')
    expect(sent).toHaveLength(1)
    expect(sent[0].range).toBe(`bytes ${CHUNK_BYTES}-${size - 1}/${size}`)
  })

  it('keeps the bytes and asks for another name when the target is taken', async () => {
    const fake = new FakeApi()
    fake.placeError = new ApiError(409, '目标已存在')
    const transfers = service(fake)
    // A refusal is not thrown at the caller that started the transfer: a drop
    // and a form submission both start one without waiting for it.
    await transfers.upload(file('kernel.bin', 4), '/guest/linux', 'linux-qemu')
    expect(transfers.getSnapshot().transfers[0]).toMatchObject({
      phase: 'needs-name',
      detail: 'HTTP 409 · 目标已存在',
    })
    expect(transfers.getSnapshot().transfers[0].file).not.toBeNull()

    fake.placeError = null
    const ok = await transfers.place(sessionId(file('kernel.bin', 4)), 'other-name')
    expect(ok).toBe(true)
    expect(transfers.getSnapshot().transfers[0]).toMatchObject({ phase: 'placed' })
  })

  it('forgets a session when it is dropped', async () => {
    const fake = new FakeApi()
    fake.offset = 0
    const transfers = service(fake)
    const id = sessionId(file('kernel.bin', 0))
    fake.placeError = new ApiError(409, '目标已存在')
    await transfers.upload(file('kernel.bin', 0), '/guest/linux', 'linux-qemu')

    await transfers.drop(id)

    expect(fake.calls.some((call) => call.method === 'DELETE')).toBe(true)
    expect(transfers.getSnapshot().transfers).toHaveLength(0)
  })

  it('notifies its subscribers with a snapshot that only changes when it does', async () => {
    const fake = new FakeApi()
    const transfers = service(fake)
    let notifications = 0
    const unsubscribe = transfers.subscribe(() => {
      notifications += 1
    })
    const before = transfers.getSnapshot()

    await transfers.refresh()
    expect(notifications).toBe(1)
    // A subscriber that reads between changes must get the same object, which is
    // what `useSyncExternalStore` compares to decide whether to re-render.
    const after = transfers.getSnapshot()
    expect(after).not.toBe(before)
    expect(transfers.getSnapshot()).toBe(after)

    unsubscribe()
    await transfers.refresh()
    expect(notifications).toBe(1)
  })
})
