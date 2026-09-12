//! Token verification: the single place in the frontend that decides whether the
//! backend accepts this token.
//!
//! The probe endpoint comes from the manifest's `auth` node (see AuthProbe in
//! types.ts), so the path is not hardcoded here and the module does not depend on
//! any specific resource. Both the token gate and a panel's danger confirmation
//! call this, instead of each rolling its own check.
//!
//! Verification and "already signed in" are two different questions: this module
//! only answers whether the token itself is usable.

import type { AuthProbe } from './types'

export type TokenVerdict =
  | { ok: true }
  | { ok: false; reason: string }

/** Probes the token once. Only an explicit backend accept counts; network errors are not treated as success. */
export async function verifyToken(
  auth: AuthProbe,
  token: string,
  signal?: AbortSignal,
): Promise<TokenVerdict> {
  let response: Response
  try {
    response = await fetch(auth.href, {
      headers: { Authorization: `${auth.scheme} ${token}` },
      signal,
    })
  } catch (e: unknown) {
    // Management API down or blocked by the network: say so instead of
    // masquerading as "wrong token".
    return { ok: false, reason: `无法连接管理接口（${auth.href}）：${String(e)}` }
  }

  if (response.ok) return { ok: true }
  if (response.status === 401 || response.status === 403) {
    return { ok: false, reason: 'token 未被接受（与构建时的 AXVM_HTTP_TOKEN 不一致）' }
  }
  return { ok: false, reason: `校验请求返回 HTTP ${response.status}` }
}
