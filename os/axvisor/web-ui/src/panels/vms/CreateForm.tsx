//! The creation form: a guest built from the fields the hypervisor declares.
//!
//! The field set is not written here. It is read from `GET /api/vms/schema`,
//! which the plane derives from the same parameters its own configuration tool
//! builds a guest from, so a field added or removed there appears here with no
//! edit. What this file owns is the two things a declaration cannot say: which
//! control a field is entered with, and how the entered text becomes a request
//! body (`./schema`).
//!
//! A `file` field is the one kind a form cannot accept as free text: its value
//! names a file in the guest filesystem, so the form offers the files that are
//! there, transfers into the named path, and keeps 创建 disabled until every
//! referenced path is placed. That is not a rule of the form's own — it is the
//! same predicate the transfer's placement and the create gate read, rendered as
//! a button state. A refusal is shown in the plane's own words, because the
//! refusals mean different things to the operator: an id that is already taken,
//! a value the plane will not accept, and a file that is not in the guest
//! filesystem yet — the last one naming the path to transfer first.

import { useEffect, useMemo, useRef, useState } from 'react'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import type { ApiClient } from '@/api/client'
import {
  describeError,
  type BrowseInfo,
  type FilesCapability,
  type PanelLink,
  type VmSchema,
  type VmSchemaField,
} from '@/api/types'
import { isMoving, useFiles } from '@/domain/files'
import { cn } from '@/lib/utils'
import { creationBody, fileFields, initialValues, splitGuestPath, unplacedFiles, type FormValues } from './schema'
import { FilePicker } from './FilePicker'

/** Matches the field controls' look; there is no select primitive in `ui/`. */
const SELECT_CLASS =
  'flex h-10 w-full rounded-md border border-input bg-background px-3 py-2 text-xs font-mono ring-offset-background focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 disabled:cursor-not-allowed disabled:opacity-50'

/** What one lookup of a guest path concluded. */
interface Placement {
  placed: boolean
  detail: string | null
}

export function CreateForm({
  api,
  link,
  files,
  open,
  onOpenChange,
  onCreated,
}: {
  api: ApiClient
  link: PanelLink
  files: FilesCapability | null
  open: boolean
  onOpenChange(open: boolean): void
  /**
   * Handed the new id so the panel can refresh the registry, and the path the
   * guest's configuration was written to (`null` when it could not be).
   */
  onCreated(id: number, savedConfig: string | null): void
}) {
  const [schema, setSchema] = useState<VmSchema | null>(null)
  const [values, setValues] = useState<FormValues>({})
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  // Field explanations come from the declaration (`description`/`example`), so
  // a first-time operator reads what the plane means — not a copy this form
  // maintains.
  const [help, setHelp] = useState(false)
  const [placements, setPlacements] = useState<Record<string, Placement>>({})
  const snapshot = useFiles(files ?? null)

  // The declaration is read when the form is opened, not when the panel loads:
  // a panel nobody creates from should not ask for a field set at all.
  useEffect(() => {
    if (!open) return
    let cancelled = false
    void (async () => {
      try {
        const declared = await api.get<VmSchema>(link.url('schema'))
        if (cancelled) return
        setSchema(declared)
        setValues(initialValues(declared))
        setError(null)
      } catch (e: unknown) {
        if (!cancelled) setError(describeError(e))
      }
    })()
    return () => {
      cancelled = true
    }
  }, [api, link, open])

  // A transfer that has just landed changes what the guest filesystem holds, so
  // the lookups below run again even though no text changed.
  const landedRef = useRef(new Set<string>())
  const [landed, setLanded] = useState(0)
  useEffect(() => {
    let fresh = false
    for (const transfer of snapshot.transfers) {
      if (transfer.phase === 'placed' && !landedRef.current.has(transfer.id)) {
        landedRef.current.add(transfer.id)
        fresh = true
      }
    }
    if (fresh) setLanded((current) => current + 1)
  }, [snapshot.transfers])

  // Whether each referenced path is in place is read from the listing, the same
  // way the picker reads it. Debounced, because typing a path must not cost one
  // request per keystroke.
  useEffect(() => {
    const targets = (schema === null ? [] : fileFields(schema))
      .map((field) => (values[field.name] ?? '').trim())
      .filter((path) => path.length > 0)
    if (targets.length === 0) {
      setPlacements({})
      return
    }
    const timer = setTimeout(() => {
      void (async () => {
        const next: Record<string, Placement> = {}
        for (const path of targets) {
          const parts = splitGuestPath(path)
          if (parts === null) {
            next[path] = { placed: false, detail: '要一个绝对路径，例如 /guest/linux/linux-qemu' }
            continue
          }
          try {
            const info = await api.get<BrowseInfo>(
              `${link.url('browse')}?path=${encodeURIComponent(parts.directory)}`,
            )
            const hit = info.files.some((entry) => entry.path === path)
            next[path] = {
              placed: hit,
              detail: hit ? null : `${parts.directory} 里没有这个文件`,
            }
          } catch (e: unknown) {
            next[path] = { placed: false, detail: describeError(e) }
          }
        }
        setPlacements(next)
      })()
    }, 300)
    return () => clearTimeout(timer)
  }, [api, link, schema, values, landed])

  // The gate only applies where the form can read the predicate at all: a build
  // without the filesystem has no listing, and the plane answers at create time.
  const canCheck = link.declared('browse')
  const unplaced = useMemo(
    () =>
      schema === null || !canCheck
        ? []
        : unplacedFiles(schema, values, (path) => placements[path]?.placed === true),
    [canCheck, placements, schema, values],
  )

  const submit = async () => {
    if (schema === null) return
    const body = creationBody(schema, values)
    if (!body.ok) {
      setError(body.error)
      return
    }
    setBusy(true)
    try {
      const created = await api.post<{ id: number; config?: string | null }>(
        link.url('create'),
        { fields: body.fields },
      )
      setError(null)
      onCreated(created.id, created.config ?? null)
      onOpenChange(false)
    } catch (e: unknown) {
      setError(describeError(e))
    } finally {
      setBusy(false)
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <div className="flex items-center justify-between gap-2">
            <DialogTitle>填表创建客户机</DialogTitle>
            <Button
              size="sm"
              variant={help ? 'secondary' : 'outline'}
              onClick={() => setHelp((current) => !current)}
            >
              字段说明
            </Button>
          </div>
          <DialogDescription>
            字段集来自 `GET /api/vms/schema`（后端模板的投影），提交为 `POST /api/vms/create`
            的 fields 请求体；地址可写十进制或 `0x8020_0000`，留空的选填项由模板填。
            不知道某个字段填什么，点「字段说明」。
          </DialogDescription>
        </DialogHeader>

        <div className="flex max-h-[55vh] flex-col gap-2 overflow-y-auto pr-1">
          {schema === null && <p className="text-sm text-muted-foreground">正在读取字段集…</p>}
          {(schema?.fields ?? []).map((field) => (
            <div key={field.name} className="flex flex-col gap-1">
              {field.type === 'file' ? (
                <FileField
                  field={field}
                  value={values[field.name] ?? ''}
                  onChange={(next) => setValues((current) => ({ ...current, [field.name]: next }))}
                  status={placements[(values[field.name] ?? '').trim()]}
                  api={api}
                  link={link}
                  files={files}
                />
              ) : (
                <label className="flex flex-col gap-1">
                  <span className="font-mono text-xs text-muted-foreground">
                    {field.name}
                    {field.type !== 'string' && <span className="ml-1">({field.type})</span>}
                    {field.required && <span className="ml-1 text-destructive">*</span>}
                  </span>
                  <FieldControl
                    field={field}
                    value={values[field.name] ?? ''}
                    onChange={(next) => setValues((current) => ({ ...current, [field.name]: next }))}
                  />
                </label>
              )}
              {help && field.description !== undefined && (
                <p className="text-xs text-muted-foreground">
                  {field.description}
                  {field.example !== undefined && (
                    <>
                      {' '}例：
                      <span className="font-mono">{String(field.example)}</span>
                    </>
                  )}
                </p>
              )}
            </div>
          ))}
          {!canCheck && (
            <p className="text-xs text-muted-foreground">
              这个构建没有文件系统能力，表单无法在这里确认文件是否就位；创建时由控制面校验。
            </p>
          )}
          {error && (
            <p className="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm">
              {error}
            </p>
          )}
        </div>

        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            取消
          </Button>
          <Button disabled={busy || schema === null || unplaced.length > 0} onClick={() => void submit()}>
            创建
          </Button>
        </DialogFooter>
        {unplaced.length > 0 && (
          <p className="text-xs text-amber-600">
            这些文件还没就位，传完才能创建：{unplaced.join('、')}
          </p>
        )}
      </DialogContent>
    </Dialog>
  )
}

/**
 * One file field: the path it names, the transfer into that path, and whether
 * the file is there.
 *
 * It behaves like a file manager's address bar plus a drop target: the path is
 * typed or picked, the placeholder is adopted as the value by Enter or by any
 * transfer, and dropping a local file on the field sends it to exactly the path
 * the field names. So "the transfer finished" and "this field is filled with a
 * placed file" are the same event, and 创建 unlocks on the facts rather than on
 * a promise — the same predicate the transfer's placement and the create gate
 * read. A refusal is shown in the plane's own words, because the refusals mean
 * different things to the operator: an id that is already taken, a value the
 * plane will not accept, and a file that is not in the guest filesystem yet —
 * the last one naming the path to transfer first.
 */
const FIELD_PLACEHOLDER = '/guest/linux/linux-qemu'

function FileField({
  field,
  value,
  onChange,
  status,
  api,
  link,
  files,
}: {
  field: VmSchemaField
  value: string
  onChange(value: string): void
  status: Placement | undefined
  api: ApiClient
  link: PanelLink
  files: FilesCapability | null
}) {
  const snapshot = useFiles(files ?? null)
  const [pickOpen, setPickOpen] = useState(false)
  const [dragOver, setDragOver] = useState(false)
  const chooser = useRef<HTMLInputElement | null>(null)

  // An empty field still has a destination: the placeholder is the suggested
  // path, and Enter or any transfer adopts it as the value.
  const effective = value.trim() || FIELD_PLACEHOLDER
  const parts = splitGuestPath(effective)
  const commit = () => {
    if (value.trim() !== effective) onChange(effective)
  }
  const transfer = useMemo(() => {
    if (parts === null) return undefined
    const forThis = snapshot.transfers.filter(
      (row) => row.directory === parts.directory && row.name === parts.name,
    )
    return forThis.find((row) => isMoving(row)) ?? forThis.at(-1)
  }, [parts, snapshot.transfers])
  const transferring = transfer !== undefined && isMoving(transfer)

  const uploadTo = (file: File) => {
    if (files === null || transferring) return
    const target = splitGuestPath(effective)
    if (target === null) return
    commit()
    void files.upload(file, target.directory, target.name)
  }

  let state: { text: string; tone: 'placed' | 'pending' | 'failed' }
  if (transferring) {
    const percent = transfer.total > 0 ? Math.round((transfer.written / transfer.total) * 100) : 0
    state = { text: `传输中 ${transfer.written} / ${transfer.total} B（${percent}%）`, tone: 'pending' }
  } else if (status?.placed) {
    state = { text: '已就位', tone: 'placed' }
  } else if (transfer?.phase === 'needs-name') {
    state = { text: `放置被拒（${transfer.detail ?? '目标已占用'}），换个路径再传`, tone: 'failed' }
  } else if (transfer?.phase === 'failed') {
    state = { text: `传输失败：${transfer.detail ?? ''}`, tone: 'failed' }
  } else if (parts === null) {
    state = { text: '要一个绝对路径，例如 /guest/linux/linux-qemu', tone: 'failed' }
  } else if (value.trim().length === 0) {
    state = { text: `回车采用默认路径，或直接把文件拖到这里上传`, tone: 'pending' }
  } else {
    state = { text: `尚未就位${status?.detail ? `：${status.detail}` : ''}`, tone: 'pending' }
  }

  return (
    <div className="flex flex-col gap-1">
      <span className="font-mono text-xs text-muted-foreground">
        {field.name}
        <span className="ml-1">(file)</span>
        {field.required && <span className="ml-1 text-destructive">*</span>}
      </span>
      {/* The whole row is the drop target, the way a file manager is: what you
          drop on it goes to the path it names. */}
      <div
        className={cn(
          'flex flex-wrap items-center gap-2 rounded-md border p-2',
          dragOver && 'border-sky-400 bg-sky-50',
        )}
        onDragOver={(event) => {
          event.preventDefault()
          if (files !== null && !transferring) setDragOver(true)
        }}
        onDragLeave={() => setDragOver(false)}
        onDrop={(event) => {
          event.preventDefault()
          setDragOver(false)
          const dropped = event.dataTransfer.files?.[0]
          if (dropped !== undefined) uploadTo(dropped)
        }}
      >
        <Input
          value={value}
          spellCheck={false}
          className="min-w-56 flex-1 font-mono text-xs"
          placeholder={FIELD_PLACEHOLDER}
          onChange={(event) => onChange(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === 'Enter') commit()
          }}
        />
        {files !== null && (
          <>
            <Button
              size="sm"
              variant="outline"
              disabled={parts === null || transferring}
              onClick={() => {
                commit()
                chooser.current?.click()
              }}
            >
              传文件…
            </Button>
            <input
              ref={chooser}
              type="file"
              className="hidden"
              onChange={(event) => {
                const chosen = event.target.files?.[0]
                if (chosen !== undefined) uploadTo(chosen)
                event.target.value = ''
              }}
            />
          </>
        )}
        {link.declared('browse') && (
          <Button size="sm" variant="outline" onClick={() => setPickOpen(true)}>
            选择已就位…
          </Button>
        )}
      </div>
      <p
        className={cn(
          'text-xs',
          state.tone === 'placed' && 'text-emerald-600',
          state.tone === 'pending' && 'text-muted-foreground',
          state.tone === 'failed' && 'text-red-600',
        )}
      >
        {state.text}
      </p>
      {files !== null && link.declared('browse') && (
        <FilePicker
          api={api}
          listing={(path) => `${link.url('browse')}?path=${encodeURIComponent(path)}`}
          makeFolder={(parent, name) => files.mkdir(parent, name)}
          open={pickOpen}
          onOpenChange={setPickOpen}
          onPick={onChange}
          initial={parts?.directory ?? '/'}
        />
      )}
    </div>
  )
}

/**
 * One field's control, chosen by the type the declaration gives it.
 *
 * An `enum` is a choice among declared options; an `enum` that declares none
 * falls back to a text field, because a select with nothing to select cannot say
 * anything. Everything else is text, which is also what a field type this build
 * does not know becomes. A control that shows a gray hint (an address, say)
 * accepts Enter as "make the hint the value", the way an address bar accepts
 * its own suggestion — the hint is what the plane expects, and typing it by
 * hand is exactly reproducing it.
 */
function FieldControl({
  field,
  value,
  onChange,
}: {
  field: VmSchemaField
  value: string
  onChange(value: string): void
}) {
  const options = field.options ?? []
  if (field.type === 'enum' && options.length > 0) {
    return (
      <select
        className={SELECT_CLASS}
        value={value}
        onChange={(event) => onChange(event.target.value)}
      >
        {options.map((option) => (
          <option key={option} value={option}>
            {option}
          </option>
        ))}
      </select>
    )
  }
  return (
    <Input
      value={value}
      spellCheck={false}
      className="font-mono text-xs"
      placeholder={field.type === 'address' ? '0x8020_0000' : ''}
      onChange={(event) => onChange(event.target.value)}
      onKeyDown={(event) => {
        if (event.key !== 'Enter' || value.trim().length > 0) return
        const hint = event.currentTarget.placeholder
        if (hint.length > 0) onChange(hint)
      }}
    />
  )
}
