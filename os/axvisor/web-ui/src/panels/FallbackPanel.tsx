//! 未知 kind 的降级视图（不变量 7）：不报错，直接把 manifest 节点渲染成 JSON。
//!
//! 前向兼容的意义：前端与后端可以是不同代——后端先挂上新 kind，
//! 老前端照样能看见它，而不是白屏。

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
