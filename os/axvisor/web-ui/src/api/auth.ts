//! Token 校验：整个前端唯一一处判断「这个 token 后端收不收」的地方。
//!
//! 探测端点来自 manifest 的 `auth` 节点（见 types.ts 的 AuthProbe），所以这里
//! 不写死路径，也不依赖具体资源。token 门与面板的危险操作都调这里，避免各写一套。
//!
//! 校验与「是否已登录」是两件事：本模块只回答 token 本身是否可用。

import type { AuthProbe } from './types'

export type TokenVerdict =
  | { ok: true }
  | { ok: false; reason: string }

/** 探测一次 token。只有后端明确接受才算通过；网络错误不当作通过。 */
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
    // 管理接口没起来 / 被网络挡住：如实说，不冒充「token 错误」
    return { ok: false, reason: `无法连接管理接口（${auth.href}）：${String(e)}` }
  }

  if (response.ok) return { ok: true }
  if (response.status === 401 || response.status === 403) {
    return { ok: false, reason: 'token 未被接受（与构建时的 AXVM_HTTP_TOKEN 不一致）' }
  }
  return { ok: false, reason: `校验请求返回 HTTP ${response.status}` }
}
