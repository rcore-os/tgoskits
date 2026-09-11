//! Degraded view for an unknown kind (invariant 7): no error, just render the
//! manifest node as JSON.
//!
//! The point is forward compatibility: frontend and backend may be of different
//! generations — the backend can expose a new kind first and an older frontend still
//! shows it instead of a blank page.

import { Badge } from '@/components/ui/badge'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import type { PanelProps } from '@/api/types'

export function FallbackPanel({ meta }: PanelProps) {
  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          {meta.title}
          <Badge variant="secondary">未注册 kind：{meta.kind}</Badge>
        </CardTitle>
        <CardDescription>
          渲染器注册表中没有这个 kind，降级为 JSON 视图——后端可以比前端新。
        </CardDescription>
      </CardHeader>
      <CardContent>
        <pre className="overflow-auto rounded-md bg-muted p-4 text-sm">
          {JSON.stringify(meta, null, 2)}
        </pre>
      </CardContent>
    </Card>
  )
}
