//! Contract types shared by the axvisor control plane and this frontend.
//!
//! `shell/` depends only on this module, never on `panels/`: a panel reaches the
//! shell through the `PanelRegistry` contract, so adding a panel never touches
//! the shell (see `panels/registry.ts`).

import type { ComponentType } from 'react'
import type { ApiClient } from './client'

/**
 * One operation of a panel, as declared by `GET /api/manifest`.
 *
 * `href` is a template: `{id}` and `{endpoint}` are filled in by the caller. A
 * panel reaches the backend through this and never through a path of its own, so
 * the hypervisor's route table and its declaration cannot drift apart without
 * the manifest changing first.
 */
export interface CapLink {
  name: string
  /** What the operation does to the resource: `read`, `write` or `stream`. */
  verb: string
  /** HTTP method of this one operation, as a string (`GET`, `POST`, `DELETE`). */
  method: string
  href: string
}

/** One panel node of `GET /api/manifest`. */
export interface PanelMeta {
  kind: string
  title: string
  /** URL namespace of this resource, e.g. `/api/vms`. Informational. */
  root: string
  /** The operations this build serves for this resource: the panel's only paths. */
  links: CapLink[]
  /**
   * Operations the panel may use, as a summary: `read`, `write`, `stream`.
   *
   * This is what the navigation shows; unlike [`CapLink`] it names no operation,
   * so the two are related but neither is derived from the other.
   */
  verbs: string[]
}

/** Values substituted into a link template. */
export type LinkParams = Record<string, string | number>

/**
 * A panel's own operations, bound by the shell.
 *
 * The shell hands each panel an accessor for its own resource only: a panel
 * cannot name another panel's operations, and it cannot spell a path itself.
 */
export interface PanelLink {
  /** URL of a declared operation. Throws when this build does not declare it. */
  url(name: string, params?: LinkParams): string
  /**
   * URL of an operation this build may legitimately lack, `null` when absent.
   * Use it for the optional halves of a resource (`fs`-only operations).
   */
  maybeUrl(name: string, params?: LinkParams): string | null
  /** Whether this build declares the operation at all. */
  declared(name: string): boolean
}

/**
 * The capability manifest (`GET /api/manifest`).
 *
 * The backend derives it from the features it was built with, so the navigation
 * follows the build: a panel that is not declared has no backing routes in this
 * binary. `proto` is additive — an older frontend degrades on unknown panels
 * rather than failing, which is what the fallback renderer is for.
 */
export interface Manifest {
  proto: number
  panels: PanelMeta[]
}

/** Context handed to every panel by the shell. */
export interface PanelProps {
  meta: PanelMeta
  api: ApiClient
  /** This panel's declared operations (`meta.links`), bound to the panel. */
  link: PanelLink
  /** Live VM registry snapshot (see `api/events.ts`); panels that do not care ignore it. */
  resources?: VmSummary[]
  /** VM the navigation asked the panel to focus, set by clicking a resource entry. */
  focusVm?: number | null
  /**
   * Shared transfer service, `null` when this build declares no `files` panel
   * (no filesystem). The shell builds the one instance from the `files`
   * resource's operations and hands it to every panel that needs it, so panels
   * observe one set of transfers without importing each other.
   */
  files?: FilesCapability | null
}

export type PanelComponent = ComponentType<PanelProps>

/**
 * Renderer registry contract. The implementation lives in `panels/registry.ts`.
 *
 * `terminalKind` is the kind the shell opens when an operator clicks a resource:
 * it is a registry fact, not a shell fact, because the shell must not know any
 * panel kind. That is what keeps adding a panel out of `shell/`.
 */
export interface PanelRegistry {
  resolve(kind: string): PanelComponent
  terminalKind?: string
}

/**
 * An HTTP failure that carries both dimensions a caller needs: the status code
 * and the backend's own `error` field, read from the body exactly once here.
 */
export class ApiError extends Error {
  readonly status: number
  readonly detail: string

  constructor(status: number, detail: string) {
    super(status === 0 ? detail : `HTTP ${status} · ${detail}`)
    this.name = 'ApiError'
    this.status = status
    this.detail = detail
  }
}

export function describeError(e: unknown): string {
  if (e instanceof ApiError) {
    // Status 0 is the client's own marker for "the request never reached the
    // hypervisor", which is a different situation from any HTTP rejection: the
    // reader is not being told what the backend thinks, they are being told
    // there is no backend to ask. That is what a stopped or restarted instance
    // looks like from the dashboard, so say it instead of showing the browser's
    // `Failed to fetch`.
    if (e.status === 0) {
      return '没有连上后端：连接中断，或后端已退出/正在重启。刷新页面；仍失败就重启实例。'
    }
    // A body the backend did not fill in still has to read as a sentence: the
    // status code alone already identifies the failure class on this API.
    const detail = e.detail.length > 0 ? e.detail : STATUS_HINTS[e.status] ?? ''
    return detail.length > 0 ? `HTTP ${e.status} · ${detail}` : `HTTP ${e.status}`
  }
  return e instanceof Error ? e.message : String(e)
}

/** What each rejection means on this API, used when the body carries no detail. */
const STATUS_HINTS: Record<number, string> = {
  400: '请求内容无法解析为一份客户机配置',
  404: '目标不存在（未注册，也不在客户机配置池里）',
  409: '当前状态不允许该操作',
  500: '宿主机侧故障，详见串口日志',
  503: '宿主资源不足（内存或浏览器终端通道已用尽）',
}

/** Human-readable VM status; an unknown future state is shown verbatim. */
export function describeStatus(status: VmStatus | string): string {
  return STATUS_TEXT[status as VmStatus] ?? String(status)
}

const STATUS_TEXT: Record<VmStatus, string> = {
  ready: '就绪',
  running: '运行中',
  pausing: '暂停中',
  paused: '已暂停',
  stopping: '停止中',
  stopped: '已停止',
  destroying: '销毁中',
  destroyed: '已销毁',
  failed: '失败',
  unknown: '未知',
}

/** VM lifecycle status strings reported by the control plane (`VmStatus::as_str`). */
export type VmStatus =
  | 'ready'
  | 'running'
  | 'pausing'
  | 'paused'
  | 'stopping'
  | 'stopped'
  | 'destroying'
  | 'destroyed'
  | 'failed'
  | 'unknown'

/** One element of `GET /api/vms`. */
export interface VmSummary {
  id: number
  name: string
  status: VmStatus
  cpu_num: number
  memory_mb: number
}

/** One element of `GET /api/vms/{id}`'s `vcpu_states`. */
export interface VcpuState {
  id: number
  state: string
  /**
   * CPU affinity as a **bitmask**, `null` when the vCPU is not pinned.
   *
   * The control plane reports AxVisor's `phys_cpu_set: Option<usize>` verbatim
   * (`control/transport/api/vm.rs`), so `0b10` means "Core 1" and nothing here is
   * a list of ids.
   * Decode it with `lib/vcpu.ts`; a `number[]` reading is a contract bug that
   * throws at render time (`.join` on a number).
   */
  phys_cpu_set: number | null
}

/** `GET /api/vms/{id}`: the summary plus per-vCPU state and the progress counters. */
export interface VmDetail extends VmSummary {
  vcpu_states?: VcpuState[]
  /** VM-level aggregate: advances only after a vCPU actually re-entered the guest. */
  guest_entry_count?: number
  /** VM-level aggregate: advances only when a vCPU genuinely parked in the suspend wait. */
  guest_park_count?: number
}

/** Body of a lifecycle action (`start`/`stop`/`pause`/`resume`). */
export interface ActionResult {
  ok: boolean
  status: VmStatus
  /**
   * `stop` and `pause` have request semantics: the response reports the status
   * right after the request was accepted, so the transition may still be in
   * flight. The UI must not treat `ok` as "converged".
   */
  async: boolean
}

/**
 * One field of `GET /api/vms/schema`, which is the creation form's field set.
 *
 * The set is the template's rather than this interface's: the hypervisor derives
 * it from the same parameters its own configuration tool builds a guest from, so
 * a form built from this cannot drift from what a creation request accepts.
 * `type` says what a value means (`integer`, `string`, `address` or `enum`),
 * `required` is the difference between a field a request must carry and one the
 * template fills, and `options` belongs to an `enum`.
 */
export interface VmSchemaField {
  name: string
  type: string
  required: boolean
  /** Value the template fills in when the request omits the field. */
  default?: string | number | null
  options?: string[]
  /** What the field means, in the plane's own words; the form's help shows it. */
  description?: string
  /** A value the operator can copy instead of inventing one. */
  example?: string | number
}

/** `GET /api/vms/schema`: the fields a creation request may carry. */
export interface VmSchema {
  fields: VmSchemaField[]
}

/** One element of `GET /api/vms/pool`'s `entries`. */
export interface PoolEntry {
  id: number
  name: string
  path: string
  /** Directory this entry was read from, which is what a conflict is between. */
  source: string
  /** The raw TOML of the entry, so a client can show or prefill it. */
  toml: string
}

/**
 * One file in the pool or browse listing that cannot become a VM, with the
 * reason the scanner rejected it (`empty`, `invalid-toml`, `unreadable`,
 * `duplicate-id`, `missing-image`, `directory-unavailable`).
 */
export interface PoolIssue {
  kind: string
  path: string
  detail: string
}

/** `GET /api/vms/pool` (`fs` builds only). */
export interface PoolInfo {
  directory: string
  /** Every directory the pool reads, in precedence order. */
  sources: string[]
  entries: PoolEntry[]
  issues: PoolIssue[]
}

/** One subdirectory of `GET /api/vms/browse`. */
export interface BrowseDirectory {
  name: string
  path: string
}

/** `GET /api/vms/browse?path=...` (`fs` builds only). */
export interface BrowseInfo {
  path: string
  /** Parent directory, or `null` at the filesystem root. */
  parent: string | null
  directories: BrowseDirectory[]
  /**
   * The plain files here. A `file` creation field's candidates come from this:
   * a path the listing holds is one that is already in the guest filesystem.
   */
  files: FolderFile[]
  /** Startable `.toml` files in this directory. */
  entries: PoolEntry[]
  /** `.toml` files here that cannot become a VM, and unreadable directories. */
  issues: PoolIssue[]
}

/** One element of `GET /api/consoles`: a WebSocket route and what it belongs to. */
export interface ConsoleInfo {
  route: string
  name: string
  /**
   * Whether a browser already holds this lane. The lanes are exclusive, so this
   * is the only way to tell an operator why their own socket was refused: a
   * browser WebSocket hides the server's 409 behind an anonymous 1006.
   */
  attached: boolean
}

/**
 * Where one staged object stands (`state` of a [`FileSession`]).
 *
 * Byte arrival and being usable are two different questions, which is why the
 * states are not collapsed into one: `uploading` is bytes arriving (and is not
 * listed at all), `uploaded` is complete bytes that are not at their target
 * yet, `placing` holds the target exclusively, and only `placed` may be
 * referenced by a config. `failed` carries a readable reason.
 */
export type FileState = 'uploading' | 'uploaded' | 'placing' | 'placed' | 'failed'

/** One element of `GET /api/files`: a transfer and the bytes it has on disk. */
export interface FileSession {
  /** Client-chosen id; re-opening it resumes the session it already names. */
  id: string
  /** Directory the bytes will land in. `place` names the file inside it. */
  directory: string
  /** Final name, known once `place` has been asked for it. */
  name: string | null
  /** Final path, known once the file is `placed`. */
  path: string | null
  /** Declared length of the whole file. */
  total: number
  /** Bytes actually on disk: the only offset a resume may continue from. */
  written: number
  state: FileState
  /** Why the transfer failed, when it did. */
  detail: string | null
}

/** `GET /api/files` (`fs` builds only). */
export interface FilesInfo {
  files: FileSession[]
}

/**
 * One transfer the client is driving.
 *
 * The reported [`FileState`] describes a session the backend knows about; this
 * is finer, because a transfer is not listed while its bytes are arriving, and a
 * staging id is chosen by the client before any session state exists.
 */
export interface Transfer {
  id: string
  /** Final name: what `place` will write the bytes as. */
  name: string
  directory: string
  written: number
  total: number
  /**
   * Local phase, which is finer than the reported state: the backend only
   * publishes `uploading` for a session it has, and never lists it.
   */
  phase: 'opening' | 'sending' | 'placing' | 'placed' | 'needs-name' | 'failed'
  detail: string | null
  /** The bytes, kept only while a resume could still need them. */
  file: File | null
}

/** What a consumer of the transfer service observes. */
export interface FilesSnapshot {
  sessions: FileSession[]
  transfers: Transfer[]
}

/**
 * The transfer service the composition root injects.
 *
 * One declaration of the boundary, implemented by the state machine in
 * `domain/files.ts` and consumed by any panel that transfers a file. The shell
 * builds a single instance so the file panel and a creation form observe the
 * same transfers.
 */
export interface FilesCapability {
  subscribe(listener: () => void): () => void
  getSnapshot(): FilesSnapshot
  /** Reads the backend's session listing. */
  refresh(): Promise<void>
  /** Sends one file into `directory`, finally named `name`. */
  upload(file: File, directory: string, name?: string): Promise<void>
  /** Continues a transfer whose bytes are still held. */
  resume(id: string): Promise<void>
  /** Moves finished bytes to `name`; `false` when the name is taken. */
  place(id: string, name: string): Promise<boolean>
  /** Forgets a session and the bytes it staged. */
  drop(id: string): Promise<void>
  /** Creates one directory level, returning the path it created. */
  mkdir(parent: string, name: string): Promise<string>
}

/** One subdirectory of `GET /api/files/browse`. */
export interface FolderDirectory {
  name: string
  path: string
}

/** One file of a folder listing (`GET /api/vms/browse`, `GET /api/files/browse`). */
export interface FolderFile {
  name: string
  path: string
  /** Length in bytes, or zero when the entry could not be measured. */
  size: number
}

/**
 * `GET /api/files/browse?path=...`: what one folder holds.
 *
 * A transfer target has to exist already, which is the whole reason this read
 * exists: it is what lets the interface walk to a folder instead of asking the
 * operator to spell one, and what lets it show what is *in* the folder being
 * looked at. Guest configs are not part of the answer — a folder view answers
 * "what is here", not "what can become a VM".
 */
export interface FolderListing {
  path: string
  /** Parent directory, or `null` at the filesystem root. */
  parent: string | null
  directories: FolderDirectory[]
  files: FolderFile[]
  /** Folders here that could not be read, and why. */
  issues: PoolIssue[]
}
