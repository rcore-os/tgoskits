//! Token gate: have the backend verify the token before the UI is entered.
//!
//! The probe path comes from the manifest's auth node — the gate hardcodes no endpoint
//! and does not assume the backend verifies tokens at all (an older backend without an
//! auth node degrades to "verified on write", and the copy says so explicitly).
//!
//! The token lives only in memory (held by App); it is never written to
//! sessionStorage or localStorage.

import { useState } from 'react'
import { verifyToken } from '@/api/auth'
import type { AuthProbe } from '@/api/types'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Input } from '@/components/ui/input'

interface TokenGateProps {
  /** Auth scheme declared by the manifest; null means the backend declared none (or the manifest has not arrived yet). */
  auth: AuthProbe | null
  /** Why the manifest fetch failed; nothing can be verified until the manifest is reachable. */
  manifestError: string | null
  onRetryManifest: () => void
  onSubmit: (token: string) => void
}

/** Copy for the gate card: branches over "manifest failed / no probe declared / probe declared", avoiding a nested ternary. */
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
      // No probe endpoint available: let it through, and be explicit that verification
      // happens on write.
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
