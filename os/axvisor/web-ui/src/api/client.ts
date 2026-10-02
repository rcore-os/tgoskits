//! REST client for the axvisor control plane.
//!
//! Paths are relative (the dashboard is served from the same origin), so the dev
//! server proxy and the bundle embedded in the hypervisor both work without a
//! build-time base URL. The method set stays generic — adding a panel does not
//! change this module.

import { useRef } from 'react'
import { ApiError } from './types'

/**
 * Status used for "the request never reached the backend".
 *
 * `fetch` rejects with a plain `TypeError` when it cannot connect at all, which
 * carries no status and reads as `Failed to fetch` in the UI. The client turns
 * that into the same `ApiError` shape every caller already handles, with status
 * 0 so `describeError` can name the situation instead of leaking the browser's
 * wording.
 */
export const NO_RESPONSE_STATUS = 0

export class ApiClient {
  get<T>(path: string, signal?: AbortSignal): Promise<T> {
    return this.request<T>('GET', path, undefined, signal)
  }

  post<T>(path: string, body?: unknown, signal?: AbortSignal): Promise<T> {
    return this.request<T>('POST', path, body, signal)
  }

  /** `DELETE` returns 204 with an empty body, so the caller gets the status only. */
  async del(path: string, signal?: AbortSignal): Promise<number> {
    const res = await send(() => fetch(path, { method: 'DELETE', signal }))
    if (!res.ok) throw await parseError(res)
    return res.status
  }

  /**
   * `PATCH` with a caller-shaped body, answering with the response headers.
   *
   * A chunk is raw bytes under a caller-built frame rather than JSON, so neither
   * the body nor the headers can be built here; and the endpoint answers in a
   * header instead of a body, so the headers are what comes back.
   */
  async patch(
    path: string,
    body: Blob,
    headers: Record<string, string>,
    signal?: AbortSignal,
  ): Promise<Headers> {
    const res = await send(() => fetch(path, { method: 'PATCH', headers, body, signal }))
    if (!res.ok) throw await parseError(res)
    return res.headers
  }

  /** `HEAD`: for endpoints that answer in headers only, with no body to parse. */
  async head(path: string, signal?: AbortSignal): Promise<Headers> {
    const res = await send(() => fetch(path, { method: 'HEAD', signal }))
    if (!res.ok) throw await parseError(res)
    return res.headers
  }

  private async request<T>(
    method: 'GET' | 'POST',
    path: string,
    body: unknown,
    signal?: AbortSignal,
  ): Promise<T> {
    const headers: Record<string, string> = { Accept: 'application/json' }
    const init: RequestInit = { method, headers, signal }
    if (body !== undefined) {
      headers['Content-Type'] = 'application/json'
      init.body = JSON.stringify(body)
    }

    const res = await send(() => fetch(path, init))
    if (!res.ok) throw await parseError(res)
    // A successful response with no body would break `res.json()`; the control
    // plane always answers with JSON on 2xx, so an empty body is treated as such.
    const text = await res.text()
    return (text.length > 0 ? JSON.parse(text) : undefined) as T
  }
}

/**
 * Runs one `fetch`, mapping a transport failure onto [`ApiError`] with
 * [`NO_RESPONSE_STATUS`]. An aborted request keeps its own error: the caller
 * cancelled it, so it is not a backend failure.
 */
async function send(request: () => Promise<Response>): Promise<Response> {
  try {
    return await request()
  } catch (error) {
    if (error instanceof DOMException && error.name === 'AbortError') throw error
    if (error instanceof TypeError) {
      throw new ApiError(NO_RESPONSE_STATUS, error.message)
    }
    throw error
  }
}

/**
 * The body of a failed response is read exactly once here, then presented as two
 * dimensions: the HTTP status and the backend's `error` field when it has one.
 */
async function parseError(res: Response): Promise<ApiError> {
  const raw = await res.text()
  let detail = raw
  try {
    const parsed: unknown = JSON.parse(raw)
    if (parsed !== null && typeof parsed === 'object' && 'error' in parsed) {
      const value = (parsed as { error: unknown }).error
      if (typeof value === 'string') detail = value
    }
  } catch {
    // Not JSON: keep the text as-is so the context is not lost.
  }
  return new ApiError(res.status, detail.trim() || res.statusText)
}

/**
 * One client instance for the lifetime of the shell: it holds no state, but
 * keeping the identity stable keeps panel effects from re-running on every render.
 */
export function useApiClient(): ApiClient {
  const ref = useRef<ApiClient | null>(null)
  if (ref.current === null) ref.current = new ApiClient()
  return ref.current
}
