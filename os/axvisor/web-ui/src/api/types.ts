//! Contract types shared by the backend and the frontend: the manifest, the
//! panel renderer contract, the VM summary, and the error object.
//! shell/ depends only on this module, never on panels/ (invariant 5).

import type { ComponentType } from 'react'
import type { ApiClient } from './client'

/**
 * One resource family node from the manifest.
 * `href` is the root of the resource; the create, detail and action routes are
 * all derived from it (href + "/{id}" and so on) — panels must not hardcode
 * endpoints.
 */
export interface ResourceMeta {
  kind: string
  title: string
  href: string
  verbs: string[]
}

/**
 * The auth scheme and token probe path declared by the manifest.
 * The client reads the probe endpoint from here instead of hardcoding it, so the
 * backend can move the path without a frontend change.
 */
export interface AuthProbe {
  href: string
  scheme: string
}

/** Backend manifest (`GET /api/`): additive after `proto`, so an older frontend degrades on unknown fields. */
export interface Manifest {
  proto: number
  /** Optional: when the backend omits it, the frontend can only fall back to validating on write. */
  auth?: AuthProbe
  resources: ResourceMeta[]
}

/** Context handed to every panel: manifest node + REST client + shell-level resource snapshot.
 *  resources/refresh inject the shell polling snapshot — resource panels (vms) use them, other panels ignore them. */
export interface PanelProps {
  meta: ResourceMeta
  token: string
  api: ApiClient
  resources?: VmInfo[]
  refresh?: () => void
  /** Auth scheme declared by the manifest; a panel uses it to re-verify the token retyped in a danger confirmation. */
  auth?: AuthProbe
}

export type PanelComponent = ComponentType<PanelProps>

/** Registry contract. The implementation lives in panels/registry.ts and is injected into the shell by the entry point. */
export interface PanelRegistry {
  resolve(kind: string): PanelComponent
}

/** VM summary (an element of `GET /api/vms`). status is an opaque string: the backend may be newer than the frontend. */
export interface VmInfo {
  id: number
  name?: string
  status: string
  cpu_num?: number
  memory_mb?: number
}

/** VM detail (`GET /api/vms/{id}`): the summary plus vCPU states and two monotonic counters. */
export interface VmDetail extends VmInfo {
  vcpu_states?: string[]
  /** Times a vCPU really entered the guest (terminal-state evidence for start/resume). */
  guest_entry_count?: number
  /** Times a vCPU really parked (terminal-state evidence for pause). */
  guest_park_count?: number
}

/**
 * Display text for the known statuses. `describeStatus` returns unknown values
 * verbatim, so a new backend status degrades to plain text instead of crashing.
 */
const STATUS_TEXT: Record<string, string> = {
  ready: '就绪',
  running: '运行中',
  pausing: '暂停中…',
  paused: '已暂停',
  stopping: '停止中…',
  stopped: '已停止',
  destroying: '销毁中…',
  destroyed: '已销毁',
  failed: '失败',
}

export function describeStatus(status: string): string {
  return STATUS_TEXT[status] ?? status
}

/**
 * Error object carrying the HTTP status. Backend error responses carry an empty
 * body (status code only), so the readable text comes from the table below.
 */
export class ApiError extends Error {
  readonly status: number
  readonly detail: string

  constructor(status: number, detail: string) {
    super(`HTTP ${status} · ${detail}`)
    this.status = status
    this.detail = detail
  }
}

const STATUS_HINT: Record<number, string> = {
  400: '请求不合法',
  401: 'token 缺失或不匹配：token 已失效，请刷新页面用构建时的 AXVM_HTTP_TOKEN 重新登录',  404: 'VM 不存在',
  409: '当前状态不允许该操作',
  500: '宿主错误，可查串口日志',
  503: '宿主资源不足',
}

export function describeError(e: unknown): string {
  if (e instanceof ApiError) {
    const hint = STATUS_HINT[e.status] ?? '请求失败'
    return e.detail ? `HTTP ${e.status} · ${e.detail}` : `HTTP ${e.status} · ${hint}`
  }
  return e instanceof Error ? e.message : String(e)
}
