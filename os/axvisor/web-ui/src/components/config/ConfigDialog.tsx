import { useEffect, useMemo, useState } from 'react'
import { AlertTriangle, Copy, Save } from 'lucide-react'
import type { RJSFSchema } from '@rjsf/utils'
import { parse } from 'smol-toml'
import type { ApiClient } from '@/api/client'
import {
  ApiError,
  describeError,
  type PanelLink,
  type PoolEntry,
  type PoolUpdateRequest,
  type PoolWriteResult,
} from '@/api/types'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { ConfigEditor } from './ConfigEditor'
import { validateConfigName, validateTomlSource } from './config-model'
import type { NormalizeConfig } from './visual-mode-request'

export type ConfigDialogTarget =
  | { kind: 'new' }
  | { kind: 'entry'; entry: PoolEntry; copy: boolean }

export interface ConfigDialogProps {
  open: boolean
  target: ConfigDialogTarget
  schema: RJSFSchema
  api: ApiClient
  link: PanelLink
  onOpenChange(open: boolean): void
  onSaved(path: string): void
  onCreated(id: number): void
  onCopy(entry: PoolEntry): void
}

export function ConfigDialog({
  open,
  target,
  schema,
  api,
  link,
  onOpenChange,
  onSaved,
  onCreated,
  onCopy,
}: ConfigDialogProps) {
  const entry = target.kind === 'entry' ? target.entry : null
  const copying = target.kind === 'entry' && target.copy
  const updating = entry !== null && !copying && entry.editable === true
  const readOnly = entry !== null && !copying && entry.editable !== true
  const [toml, setToml] = useState('')
  const [name, setName] = useState('')
  const [dirty, setDirty] = useState(false)
  const [valid, setValid] = useState(true)
  const [submitting, setSubmitting] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const initialToml = entry?.toml ?? ''
  const needsName = !updating
  const initialMode = target.kind === 'new' ? 'visual' : 'source'
  const normalizeUrl = link.maybeUrl('config_normalize')
  const normalize = useMemo(() => createConfigNormalizer(api, normalizeUrl), [api, normalizeUrl])

  useEffect(() => {
    if (!open) return
    setToml(initialToml)
    setName('')
    setDirty(false)
    setValid(true)
    setSubmitting(false)
    setError(null)
  }, [initialToml, open, target])

  const title = useMemo(() => {
    if (target.kind === 'new') return '新建客户机配置'
    if (target.copy) return `复制配置：${target.entry.name}`
    return target.entry.editable === true ? `编辑配置：${target.entry.name}` : `查看配置：${target.entry.name}`
  }, [target])

  const requestClose = () => {
    if (submitting) return
    if (dirty && !window.confirm('配置还有未保存的修改，确定放弃吗？')) return
    onOpenChange(false)
  }

  const save = async () => {
    if (submitting || !valid) return
    const sourceError = validateTomlSource(toml)
    if (sourceError !== null) {
      setError(sourceError)
      return
    }
    setSubmitting(true)
    setError(null)
    try {
      let saved: PoolWriteResult
      if (updating && entry !== null) {
        const url = link.maybeUrl('pool_update')
        if (url === null) throw new Error('当前后端没有声明配置更新能力，请改用「另存为」')
        const body: PoolUpdateRequest = {
          path: entry.path,
          expected_toml: entry.toml,
          toml,
        }
        saved = await api.post<PoolWriteResult>(url, body)
      } else {
        const nameError = validateConfigName(name)
        if (nameError !== null) {
          setError(nameError)
          return
        }
        const url = link.maybeUrl('pool_save')
        if (url === null) throw new Error('当前后端没有声明配置库写入能力')
        saved = await api.post<PoolWriteResult>(url, { name: name.trim(), toml })
      }
      setDirty(false)
      onSaved(saved.path)
      onOpenChange(false)
    } catch (caught: unknown) {
      setError(poolError(caught))
    } finally {
      setSubmitting(false)
    }
  }

  const createGuest = async () => {
    if (submitting || !valid) return
    const sourceError = validateTomlSource(toml)
    if (sourceError !== null) {
      setError(sourceError)
      return
    }
    const parsed = parse(toml, {
      integersAsBigInt: 'asNeeded',
      unsafeKeyBehaviour: 'throw',
    })
    const id = parsed.id ?? (isRecord(parsed.base) ? parsed.base.id : undefined)
    if (id === undefined || id === null || id === '') {
      setError('完整配置创建客户机时必须显式填写 id；快速创建才会自动分配')
      return
    }
    // Submit the original TOML, not a JSON/Schema conversion. Source mode may
    // contain exact integers and dates that browser controls cannot represent;
    // the backend owns final configuration validation.
    setSubmitting(true)
    setError(null)
    try {
      const created = await api.post<{ id: number }>(link.url('create'), { toml })
      setDirty(false)
      onCreated(created.id)
      onOpenChange(false)
    } catch (caught: unknown) {
      setError(describeError(caught))
    } finally {
      setSubmitting(false)
    }
  }

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next) requestClose()
      }}
    >
      <DialogContent
        className="flex h-[min(92vh,58rem)] w-[min(96vw,78rem)] max-w-none flex-col gap-0 overflow-hidden p-0"
        onEscapeKeyDown={(event) => {
          if (submitting || dirty) {
            event.preventDefault()
            requestClose()
          }
        }}
        onPointerDownOutside={(event) => {
          if (submitting || dirty) {
            event.preventDefault()
            requestClose()
          }
        }}
      >
        <DialogHeader className="border-b px-5 py-4 pr-12">
          <DialogTitle>{title}</DialogTitle>
          <DialogDescription className="mt-1 flex flex-wrap items-center gap-x-3 gap-y-1">
            {entry === null ? (
              '从完整 Schema 开始填写；没有暗中套用任何模板。'
            ) : (
              <>
                <span className="font-mono text-xs">{entry.path}</span>
                <span>保存配置不会改变已经注册或正在运行的客户机。</span>
              </>
            )}
          </DialogDescription>
        </DialogHeader>

        {readOnly && (
          <div className="mx-5 mt-3 flex items-start gap-2 rounded-md border border-warn/40 bg-warn/10 px-3 py-2 text-sm">
            <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0 text-warn" />
            此来源不可编辑。可以查看或用「另存为」复制到可写配置库。
          </div>
        )}

        <ConfigEditor
          key={`${target.kind}-${entry?.path ?? 'new'}-${target.kind === 'entry' && target.copy ? 'copy' : 'direct'}`}
          schema={schema}
          initialToml={initialToml}
          readOnly={readOnly}
          initialMode={initialMode}
          active={open}
          normalize={normalize}
          onChange={(next, changed) => {
            setToml(next)
            setDirty(changed || (needsName && name.trim().length > 0))
            setError(null)
          }}
          onValidityChange={setValid}
        />

        <div className="border-t bg-background px-5 py-3">
          {needsName && (
            <div className="mb-3 grid gap-1.5 sm:grid-cols-[9rem_minmax(0,24rem)] sm:items-center">
              <label htmlFor="config-name" className="text-sm font-medium">
                目标文件名
              </label>
              <Input
                id="config-name"
                value={name}
                disabled={submitting}
                placeholder="例如 alpine.toml"
                className="font-mono text-sm"
                onChange={(event) => {
                  setName(event.target.value)
                  setDirty(event.target.value.trim().length > 0 || toml !== initialToml)
                  setError(null)
                }}
              />
            </div>
          )}
          {error !== null && (
            <p role="alert" className="mb-3 rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm">
              {error}
            </p>
          )}
          <div className="flex flex-col-reverse gap-2 sm:flex-row sm:items-center sm:justify-end">
            <Button variant="ghost" disabled={submitting} onClick={requestClose}>
              {readOnly ? '关闭' : '取消'}
            </Button>
            {readOnly && (
              <Button
                variant="outline"
                disabled={submitting}
                onClick={() => {
                  if (entry !== null) onCopy(entry)
                }}
              >
                <Copy className="mr-1.5 h-3.5 w-3.5" />
                另存为
              </Button>
            )}
            {!readOnly && (
              <Button
                variant="outline"
                disabled={submitting || !valid || toml.trim().length === 0 || (needsName && name.trim().length === 0)}
                onClick={() => void save()}
              >
                <Save className="mr-1.5 h-3.5 w-3.5" />
                {submitting ? '保存中…' : updating ? '保存更改' : '保存到配置库'}
              </Button>
            )}
            <Button disabled={submitting || !valid || toml.trim().length === 0} onClick={() => void createGuest()}>
              {submitting ? '创建中…' : '用此配置创建客户机'}
            </Button>
          </div>
        </div>
      </DialogContent>
    </Dialog>
  )
}

/** Bind the optional capability here; the editor never knows an API path. */
export function createConfigNormalizer(api: ApiClient, url: string | null): NormalizeConfig | undefined {
  if (url === null) return undefined
  return async (toml, signal) => {
    const result = await api.post<{ toml: string }>(url, { toml }, signal)
    if (typeof result?.toml !== 'string') throw new Error('后端未返回规范 TOML')
    return result.toml
  }
}

function poolError(error: unknown): string {
  if (error instanceof ApiError) {
    if (error.status === 409) {
      return `配置已被其他操作修改，未覆盖当前文件。请保留本草稿，刷新配置库后再处理差异。${error.detail ? ` ${error.detail}` : ''}`
    }
    if (error.status === 403) return `这个来源不可写：${error.detail}`
    if (error.status === 404) return `原配置已不存在：${error.detail}`
    if (error.status === 400) return `配置或文件名无效：${error.detail}`
  }
  return describeError(error)
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
}
