//! 前后端契约类型：manifest / 面板渲染器契约 / VM 摘要 / 错误对象。
//! shell/ 只依赖这里，不依赖 panels/（不变量 5）。

import type { ComponentType } from 'react'
import type { ApiClient } from './client'

/**
 * manifest 里的一个资源族节点。
 * `href` 是该资源的根，create/详情/动作路由都由它派生（href + "/{id}" 等）——
 * 面板不得硬编码端点。
 */
export interface ResourceMeta {
  kind: string
  title: string
  href: string
  verbs: string[]
}

/**
 * manifest 声明的鉴权方式与 token 探测路径。
 * 客户端从这里取探测端点，不硬编码——后端换路径前端不必改。
 */
export interface AuthProbe {
  href: string
  scheme: string
}

/** 后端 manifest（`GET /api/`）：proto 之后只增不改，老前端对未知字段降级。 */
export interface Manifest {
  proto: number
  /** 可选：后端未声明时前端只能退化为「写入时才校验」。 */
  auth?: AuthProbe
  resources: ResourceMeta[]
}

/** 每个面板拿到的上下文：manifest 节点 + REST client + 壳级资源快照。
 *  resources/refresh 是壳级轮询快照的注入——资源型面板（vms）使用，其它面板忽略。 */
export interface PanelProps {
  meta: ResourceMeta
  token: string
  api: ApiClient
  resources?: VmInfo[]
  refresh?: () => void
  /** manifest 声明的鉴权方式；面板做危险操作确认时用它校验重输的 token。 */
  auth?: AuthProbe
}

export type PanelComponent = ComponentType<PanelProps>

/** 注册表契约。具体实现在 panels/registry.ts，由入口注入进壳。 */
export interface PanelRegistry {
  resolve(kind: string): PanelComponent
}

/** VM 摘要（`GET /api/vms` 的元素）。status 是不透明字符串：后端可以比前端新。 */
export interface VmInfo {
  id: number
  name?: string
  status: string
  cpu_num?: number
  memory_mb?: number
}

/** VM 详情（`GET /api/vms/{id}`）：摘要 + vCPU 状态与两个单调计数器。 */
export interface VmDetail extends VmInfo {
  vcpu_states?: string[]
  /** vCPU 真正进入 guest 的次数（start/resume 的终态证据）。 */
  guest_entry_count?: number
  /** vCPU 真正 park 的次数（pause 的终态证据）。 */
  guest_park_count?: number
}

/**
 * 已知状态的中文文案。`describeStatus` 对未知值原样返回，
 * 所以后端新增状态时界面降级显示而不是崩。
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
 * 错误对象携带 HTTP 状态码。后端错误响应体为空（只回状态码），
 * 所以可读文案来自下面的映射表。
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
  401: 'token 缺失或不匹配：用右上角「重设 token」重新输入构建时的 AXVM_HTTP_TOKEN',  404: 'VM 不存在',
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
