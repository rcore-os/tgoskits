//! Per-tab error boundary.
//!
//! A panel is the only place where third-party-shaped data meets rendering: the
//! JSON comes from the hypervisor, so one wrong field type is enough to throw
//! during render. React handles that by unmounting the whole root, which turns
//! a single bad panel into a blank page with no way back — that is exactly how a
//! `phys_cpu_set` type mismatch presented itself. The boundary keeps the failure
//! inside the tab that caused it: the navigation, the other tabs and the event
//! feed keep working, and the tab explains itself.
//!
//! The explanation is classified instead of assumed: a panel also fails when the
//! hypervisor is simply not there any more, and a chunk load is not a contract
//! violation. See `@/lib/panel-error`.

import { Component, type ErrorInfo, type ReactNode } from 'react'
import { Button } from '@/components/ui/button'
import { classifyPanelFailure, describePanelFailure } from '@/lib/panel-error'

interface PanelErrorBoundaryProps {
  /** Panel title, so the message names what broke. */
  title: string
  children: ReactNode
}

interface PanelErrorBoundaryState {
  error: Error | null
}

export class PanelErrorBoundary extends Component<
  PanelErrorBoundaryProps,
  PanelErrorBoundaryState
> {
  state: PanelErrorBoundaryState = { error: null }

  static getDerivedStateFromError(error: Error): PanelErrorBoundaryState {
    return { error }
  }

  componentDidCatch(error: Error, info: ErrorInfo): void {
    // The console is all this build has: the browser panel has no telemetry
    // channel, and the hypervisor only sees the requests, not the render.
    console.error(`panel ${this.props.title} crashed`, error, info.componentStack)
  }

  private readonly retry = (): void => this.setState({ error: null })

  render(): ReactNode {
    const { error } = this.state
    if (error === null) return this.props.children
    return (
      <div className="rounded-md border border-destructive/40 bg-destructive/10 p-4 text-sm">
        <p className="font-semibold text-destructive">
          面板「{this.props.title}」渲染失败，其余界面仍可用。
        </p>
        <p className="mt-1 font-mono text-xs text-muted-foreground">{String(error.message)}</p>
        <p className="mt-2 text-muted-foreground">{describePanelFailure(classifyPanelFailure(error))}</p>
        <Button className="mt-3" size="sm" variant="outline" onClick={this.retry}>
          重试渲染
        </Button>
      </div>
    )
  }
}
