//! The frontend's only way to name an operation: derived from the manifest.
//!
//! The hypervisor declares what this build serves (`GET /api/manifest`), and
//! this module turns that declaration into accessors. A panel asks for an
//! operation by name and gets a URL, so no path literal survives in `panels/`:
//! if a route is renamed, added or dropped, the declaration changes and the
//! frontend follows without an edit.
//!
//! Two failure modes are deliberately asymmetric:
//!
//! - `url` throws when the operation is not declared. A panel asking for
//!   something this build does not serve is a contract bug, and it has to be
//!   visible through the panel's error boundary instead of turning into a 404
//!   that reads as "the backend is broken".
//! - `maybeUrl` returns `null` instead. That is for operations a build may
//!   legitimately lack (`fs`-only ones): the panel renders the absent feature
//!   instead of an error.
//!
//! Nothing here knows what a panel is *about*: the module is pure and takes the
//! manifest as input, which is what makes it testable without a browser.

import type { CapLink, LinkParams, PanelLink, PanelMeta } from '@/api/types'

/** Template placeholder, e.g. `{id}` in `/api/vms/{id}`. */
const PLACEHOLDER = /\{([A-Za-z_][A-Za-z0-9_]*)\}/g

/**
 * The declared capabilities of one build.
 *
 * Instances are immutable, and `bind` hands out one cached accessor per kind, so
 * a panel can keep `link` in an effect's dependency list without re-fetching on
 * every render.
 */
export class Capabilities {
  private readonly panels: Map<string, PanelMeta>
  private readonly bound = new Map<string, PanelLink>()

  constructor(panels: PanelMeta[]) {
    this.panels = new Map(panels.map((panel) => [panel.kind, panel]))
  }

  /** Declared kinds, in declaration order. */
  kinds(): string[] {
    return [...this.panels.keys()]
  }

  /** Declared node of one kind, `null` when this build has no such panel. */
  panel(kind: string): PanelMeta | null {
    return this.panels.get(kind) ?? null
  }

  /** Declared summary of one kind's operations; empty when the kind is absent. */
  verbs(kind: string): string[] {
    return this.panel(kind)?.verbs ?? []
  }

  /** Whether `kind` declares an operation called `name`. */
  declared(kind: string, name: string): boolean {
    return findLink(this.panel(kind), name) !== null
  }

  /**
   * URL of a declared operation.
   *
   * Throws when this build declares neither the panel nor the operation, naming
   * what it does declare so the mismatch can be read off the screen.
   */
  url(kind: string, name: string, params?: LinkParams): string {
    const panel = this.panels.get(kind)
    if (!panel) {
      throw new Error(
        `能力声明里没有「${kind}」这个面板（本构建有：${this.kinds().join('、') || '无'}）`,
      )
    }
    const link = findLink(panel, name)
    if (!link) {
      const declared = panel.links.map((entry) => entry.name).join('、') || '无'
      throw new Error(`能力声明里「${kind}」没有「${name}」这个动作（本构建有：${declared}）`)
    }
    return fill(link.href, name, params)
  }

  /** URL of an operation this build may legitimately lack; `null` when absent. */
  maybeUrl(kind: string, name: string, params?: LinkParams): string | null {
    const link = findLink(this.panel(kind), name)
    return link ? fill(link.href, name, params) : null
  }

  /**
   * The operations of one kind, as the shell hands them to that panel.
   *
   * Every panel is bound to its own resource, so a panel cannot reach into
   * another one; the accessor identity is stable for the lifetime of this
   * instance.
   */
  bind(kind: string): PanelLink {
    const cached = this.bound.get(kind)
    if (cached) return cached
    if (!this.panels.has(kind)) {
      throw new Error(`无法为未声明的面板「${kind}」绑定动作`)
    }
    const bound: PanelLink = {
      url: (name, params) => this.url(kind, name, params),
      maybeUrl: (name, params) => this.maybeUrl(kind, name, params),
      declared: (name) => this.declared(kind, name),
    }
    this.bound.set(kind, bound)
    return bound
  }
}

function findLink(panel: PanelMeta | null, name: string): CapLink | null {
  return panel?.links.find((link) => link.name === name) ?? null
}

/**
 * Substitutes the template parameters of one href.
 *
 * A missing parameter is a caller bug (the template says what it needs), and a
 * leftover placeholder would be sent as a literal `{...}` in the request path,
 * so both are errors rather than a half-filled URL. Values are percent-encoded:
 * they land in a path segment, not in a query string.
 */
function fill(href: string, name: string, params: LinkParams = {}): string {
  return href.replace(PLACEHOLDER, (_match, key: string) => {
    const value = params[key]
    if (value === undefined) {
      throw new Error(`动作「${name}」的路径 ${href} 需要参数「${key}」`)
    }
    return encodeURIComponent(String(value))
  })
}
