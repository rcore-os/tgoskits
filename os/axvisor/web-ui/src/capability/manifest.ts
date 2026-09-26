//! The bootstrap path, and the validation that comes with it.
//!
//! Every other path the dashboard calls is read out of the manifest, so exactly
//! one path literal exists in this frontend: the one a client needs before it
//! has anything to read. It is the client half of `MANIFEST_PATH` on the
//! hypervisor side (`os/axvisor/src/control/capability/mod.rs`), and it cannot
//! come from the manifest itself.

import type { ApiClient } from '@/api/client'
import type { Manifest } from '@/api/types'

/** Path of `GET /api/manifest`: the single path this frontend spells out. */
export const MANIFEST_PATH = '/api/manifest'

/**
 * Protocol version this frontend reads.
 *
 * `proto` is additive for *panels*, but not for the fields a panel reaches the
 * backend through: a build whose links array has a different shape would render
 * broken panels, which is harder to diagnose than a refusal. So an unknown
 * version fails loudly here.
 */
const SUPPORTED_PROTO = 1

/**
 * Reads the manifest and refuses a version this frontend cannot drive.
 *
 * The error text names the two versions, because the fix is to rebuild one of
 * them and the reader needs to know which side is older.
 */
export async function loadManifest(api: ApiClient, signal?: AbortSignal): Promise<Manifest> {
  const manifest = await api.get<Manifest>(MANIFEST_PATH, signal)
  if (manifest.proto !== SUPPORTED_PROTO) {
    throw new Error(
      `能力声明协议版本是 ${manifest.proto}，本前端只读 ${SUPPORTED_PROTO}：请重新构建前端或后端`,
    )
  }
  if (!Array.isArray(manifest.panels)) {
    throw new Error('能力声明里没有 panels 数组：后端返回的不是本前端认识的契约')
  }
  return manifest
}
