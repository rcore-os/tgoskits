//! token 门：进入界面前先让后端校验 token。
//!
//! 探测路径来自 manifest 的 auth 节点——门不硬编码端点，也不假设后端一定有
//! token 校验（老后端没有 auth 节点时退化为「写入时才校验」，并把这句话说清楚）。
//!
//! token 只存在内存态（由 App 持有），不落 sessionStorage/localStorage。

import { useState } from 'react'
import { verifyToken } from '@/api/auth'
import type { AuthProbe } from '@/api/types'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Input } from '@/components/ui/input'

interface TokenGateProps {
  /** manifest 声明的鉴权方式；null 表示后端没声明（或清单还没取到）。 */
  auth: AuthProbe | null
  /** manifest 拉取失败的原因；此时无从校验，只能先修清单可达性。 */
  manifestError: string | null
  onRetryManifest: () => void
  onSubmit: (token: string) => void
}

/** 进门卡片的说明文案：按「清单失败 / 未声明校验 / 已声明校验」三态分支，避免嵌套三元。 */
function gateDescription(manifestError: string | null, auth: AuthProbe | null) {
  if (manifestError) {
    return <>无法读取能力清单，界面无法确定入口与鉴权方式。</>
  }
  if (auth === null) {
    return (
      <>
        管理接口需要 token：填入与构建时{' '}
        <code className="font-mono">AXVM_HTTP_TOKEN</code> 一致的值。后端未声明
        校验端点，token 会在写入时才被检验。
      </>
    )
  }
  return (
    <>
      管理接口需要 token：填入与构建时{' '}
      <code className="font-mono">AXVM_HTTP_TOKEN</code> 一致的值，进入前会向后端
      校验一次。
    </>
  )
}

export function TokenGate({ auth, manifestError, onRetryManifest, onSubmit }: TokenGateProps) {
  const [value, setValue] = useState('')
  const [failure, setFailure] = useState<string | null>(null)
  const [checking, setChecking] = useState(false)

  const submit = async () => {
    const token = value.trim()
    if (!token) return
    setFailure(null)

    if (auth === null) {
      // 没有探测端点可用：只能放行，并把「校验发生在写入时」讲清楚。
      onSubmit(token)
      return
    }

    setChecking(true)
    const verdict = await verifyToken(auth, token)
    setChecking(false)
    if (verdict.ok) {
      onSubmit(token)
    } else {
      setFailure(verdict.reason)
    }
  }

  return (
    <div className="flex min-h-screen items-center justify-center bg-muted/40 p-4">
      <Card className="w-full max-w-md">
        <CardHeader>
          <CardTitle>Axvisor</CardTitle>
        <CardDescription>{gateDescription(manifestError, auth)}</CardDescription>
        </CardHeader>
        <CardContent className="space-y-3">
          {manifestError && (
            <div className="space-y-2">
              <p className="font-mono text-xs text-destructive">{manifestError}</p>
              <Button size="sm" variant="outline" onClick={onRetryManifest}>
                重试读取能力清单
              </Button>
            </div>
          )}

          <form
            className="flex gap-2"
            onSubmit={(e) => {
              e.preventDefault()
              void submit()
            }}
          >
            <Input
              autoFocus
              value={value}
              onChange={(e) => {
                setValue(e.target.value)
                setFailure(null)
              }}
              placeholder="Bearer token"
              aria-label="token"
              disabled={manifestError !== null}
            />
            <Button type="submit" disabled={manifestError !== null || checking || !value.trim()}>
              {checking ? '校验中…' : '进入'}
            </Button>
          </form>

          {failure && (
            <p className="font-mono text-xs text-destructive" role="alert">
              {failure}
            </p>
          )}
        </CardContent>
      </Card>
    </div>
  )
}
