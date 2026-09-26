//! Classification of a panel failure, so the tab can say something true.
//!
//! A tab fails in more than one way, and they need different advice:
//!
//! - The dashboard talks to the hypervisor over HTTP and loads each panel as a
//!   separate chunk with its own stylesheet. When the hypervisor is gone (or the
//!   network is), those requests fail and the tab cannot even mount. Nothing is
//!   wrong with the code or the JSON, and retrying while the backend is down
//!   fails again.
//! - When the hypervisor is up, the panel renders data it did not choose: one
//!   field whose JSON type differs from `src/api/types.ts` throws while
//!   rendering. That is the `phys_cpu_set` failure, and it is the case the
//!   contract hint is for.
//!
//! Telling the two apart matters because the first one is a "the other side is
//! not there" condition the reader can act on, while the second one is a bug to
//! report with the payload.

/** What kind of failure a panel hit. */
export type PanelFailureKind = 'resource' | 'contract' | 'unknown'

/**
 * Messages browsers and bundlers produce when a chunk or its stylesheet cannot
 * be fetched. They surface as `Error`s from the dynamic import, not as `TypeError`
 * from rendering, but the wording is not standardized, so match the shapes
 * observed from Chromium plus the bundler's own names.
 */
const RESOURCE_FAILURE_PATTERNS: RegExp[] = [
  /unable to preload css/i,
  /failed to fetch dynamically imported module/i,
  /error loading dynamically imported module/i,
  /importing a module script failed/i,
  /chunkloaderror/i,
  /failed to fetch/i,
  /networkerror/i,
  /load failed/i,
]

/** Names a browser uses when a request never reached a server. */
const CONNECTION_FAILURE_PATTERNS: RegExp[] = [
  /\berr_connection_refused\b/i,
  /\berr_connection_reset\b/i,
  /\berr_network_changed\b/i,
  /connection refused/i,
  /network is unreachable/i,
  /socket hang up/i,
]

function messageOf(error: unknown): string {
  if (error instanceof Error) return `${error.name}: ${error.message}`
  if (typeof error === 'string') return error
  try {
    return String(error)
  } catch {
    return ''
  }
}

/**
 * Classify one panel failure.
 *
 * Resource and connection patterns are checked before the `TypeError` check: a
 * failed `fetch` also throws `TypeError`, so checking the constructor first
 * would report a dead backend as a contract violation.
 */
export function classifyPanelFailure(error: unknown): PanelFailureKind {
  const message = messageOf(error)
  for (const pattern of RESOURCE_FAILURE_PATTERNS) {
    if (pattern.test(message)) return 'resource'
  }
  for (const pattern of CONNECTION_FAILURE_PATTERNS) {
    if (pattern.test(message)) return 'resource'
  }
  if (error instanceof TypeError) return 'contract'
  return 'unknown'
}

/** What the tab should suggest for `kind`. */
export function describePanelFailure(kind: PanelFailureKind): string {
  switch (kind) {
    case 'resource':
      return '面板的代码或样式没能从后端取回：后端可能已经退出，或者连接中断。确认后端仍在运行后刷新页面（必要时重启实例）再试。'
    case 'contract':
      return '多半是后端某个字段的类型与前端契约不一致（后端 JSON 与 `src/api/types.ts`）。串口日志里有完整堆栈。'
    case 'unknown':
      return '打开浏览器控制台可以看到完整堆栈，后端串口日志里有对应的请求记录。'
  }
}
