//! REST client。
//!
//! - baseUrl 用相对路径（不变量 10）：dev 靠 vite 代理，build 产物同源直连，
//!   同一份 dist 在多形态下通用。
//! - 后端写路由要 `Authorization: Bearer <构建时 AXVM_HTTP_TOKEN>`；GET 开放，
//!   带上无妨。
//! - 错误响应体为空（只回状态码），所以 ApiError 只带状态码，文案由
//!   `describeError` 的状态码映射负责。

import { useRef } from 'react'
import { ApiError } from './types'

export class ApiClient {
  // scheme 默认 Bearer，但优先用 manifest 声明的 auth.scheme（见 types.ts 的 AuthProbe），
  // 与 api/auth.ts 的探测保持一致——不再写死。
  constructor(
    private readonly getToken: () => string,
    private readonly getScheme: () => string = () => 'Bearer',
  ) {}

  get<T>(path: string, signal?: AbortSignal): Promise<T> {
    return this.request<T>('GET', path, undefined, signal)
  }

  /** 生命周期动作（start/stop/pause/resume）没有请求体。 */
  post<T>(path: string, body?: unknown, signal?: AbortSignal): Promise<T> {
    return this.request<T>('POST', path, body, signal)
  }

  /** 删除成功是 204 无响应体，调用方拿不到 JSON。 */
  delete<T = void>(path: string, signal?: AbortSignal): Promise<T> {
    return this.request<T>('DELETE', path, undefined, signal)
  }

  private async request<T>(
    method: 'GET' | 'POST' | 'DELETE',
    path: string,
    body: unknown,
    signal?: AbortSignal,
  ): Promise<T> {
    const headers: Record<string, string> = {
      Authorization: `${this.getScheme()} ${this.getToken()}`,
    }
    const init: RequestInit = { method, headers, signal }
    if (body !== undefined) {
      headers['Content-Type'] = 'application/json'
      init.body = JSON.stringify(body)
    }

    const res = await fetch(path, init)
    // 不变量 11：body 只读一次——204/空 body 与 JSON 都从这一份文本派生。
    const raw = await res.text()
    if (!res.ok) {
      throw new ApiError(res.status, raw.trim())
    }
    return (raw ? JSON.parse(raw) : undefined) as T
  }
}

/**
 * 不变量 12：client 只在 ref 为 null 时构造一次，不随每次渲染重建；
 * token 经 getter 读取，配合 tokenRef 每次渲染刷新，读到的永远是最新的。
 */
export function useApiClient(token: string, scheme?: string): ApiClient {
  const clientRef = useRef<ApiClient | null>(null)
  const tokenRef = useRef(token)
  tokenRef.current = token
  const schemeRef = useRef(scheme)
  schemeRef.current = scheme

  if (clientRef.current === null) {
    // scheme 经 getter 读取：与 token 一样，每次请求都取最新（manifest 可能晚到）。
    clientRef.current = new ApiClient(
      () => tokenRef.current,
      () => schemeRef.current ?? 'Bearer',
    )
  }
  return clientRef.current
}
