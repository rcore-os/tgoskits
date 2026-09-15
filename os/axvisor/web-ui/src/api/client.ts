//! REST client.
//!
//! - Base URLs are relative (invariant 10): dev goes through the vite proxy while
//!   the build output is served same-origin, so one dist serves every form.
//! - Backend write routes require `Authorization: Bearer <build-time AXVM_HTTP_TOKEN>`;
//!   GET is open, and sending the header anyway is harmless.
//! - Error responses carry an empty body (status code only), so ApiError carries
//!   just the status and `describeError` maps it to readable text.

import { useRef } from 'react'
import { ApiError } from './types'

export class ApiClient {
  // Defaults to Bearer, but prefers the auth.scheme declared by the manifest (see
  // AuthProbe in types.ts) so it stays consistent with the api/auth.ts probe —
  // the scheme is no longer hardcoded.
  constructor(
    private readonly getToken: () => string,
    private readonly getScheme: () => string = () => 'Bearer',
  ) {}

  get<T>(path: string, signal?: AbortSignal): Promise<T> {
    return this.request<T>('GET', path, undefined, signal)
  }

  /** Lifecycle actions (start/stop/pause/resume) carry no request body. */
  post<T>(path: string, body?: unknown, signal?: AbortSignal): Promise<T> {
    return this.request<T>('POST', path, body, signal)
  }

  /** A successful delete is 204 with no body, so the caller gets no JSON back. */
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
    // Invariant 11: the body is read exactly once — 204/empty bodies and JSON are
    // both derived from this single piece of text.
    const raw = await res.text()
    if (!res.ok) {
      throw new ApiError(res.status, raw.trim())
    }
    return (raw ? JSON.parse(raw) : undefined) as T
  }
}

/**
 * Invariant 12: the client is constructed once, only when the ref is null, rather
 * than rebuilt on every render; the token is read through a getter backed by
 * tokenRef refreshed each render, so it always observes the latest value.
 */
export function useApiClient(token: string, scheme?: string): ApiClient {
  const clientRef = useRef<ApiClient | null>(null)
  const tokenRef = useRef(token)
  tokenRef.current = token
  const schemeRef = useRef(scheme)
  schemeRef.current = scheme

  if (clientRef.current === null) {
    // The scheme is read through a getter too: like the token, every request picks
    // up the latest value (the manifest may arrive late).
    clientRef.current = new ApiClient(
      () => tokenRef.current,
      () => schemeRef.current ?? 'Bearer',
    )
  }
  return clientRef.current
}
