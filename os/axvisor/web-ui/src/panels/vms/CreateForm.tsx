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
//!
//! Which fields are read first and which are folded into 「高级设置」 is the
//! declaration's answer too (`advanced`), as are the name a field is shown under
//! (`label`) and the example a file field hints at (`example`). What this file
//! owns is only how one field is entered: text or an address, a choice among the
//! declared options, 是/否 for a boolean, and the file row.
//!
//! What the form *opens with* comes from the plane as well: `GET /api/vms/defaults`
//! publishes the values a fresh guest of this build starts as, keyed by the same
//! field names, and the fields' own declared defaults remain only as the fallback
//! for a build that serves no such endpoint. Both reads are made together when the
//! dialog opens, and 创建 waits for both — the two are one answer to "what will
//! this request carry". An id is no longer invented here: the field opens empty
//! and the request omits it, which is how the plane assigns one, while a typed id
//! is still sent verbatim for an operator who wants a particular one.

import { useEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import { ChevronRight } from 'lucide-react'
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
  type VmDefaults,
  type VmSchema,
  type VmSchemaField,
} from '@/api/types'
import { isMoving, sessionId, useFiles } from '@/domain/files'
import { cn } from '@/lib/utils'
import { FilePicker } from './FilePicker'
import {
  acceptMatches,
  advancedFields,
  basicFields,
  creationBody,
  exampleHint,
  fieldLabel,
  fileFields,
  initialValues,
  joinGuestPath,
  splitGuestPath,
  transferDestination,
  unplacedFiles,
  type FormValues,
} from './schema'

/** Matches the field controls' look; there is no select primitive in `ui/`. */
const SELECT_CLASS =
  'flex h-10 w-full rounded-md border border-input bg-background px-3 py-2 text-xs font-mono ring-offset-background focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 disabled:cursor-not-allowed disabled:opacity-50'

/** What one lookup of a guest path concluded. */
interface Placement {
  placed: boolean
  /** The looked-up path names an existing directory, not a file. */
  isDirectory?: boolean
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
  /**
   * What this build starts a fresh guest as, `null` until it arrives.
   *
   * The creation request's values come from here rather than from the fields'
   * declared defaults: the plane is the one that knows what a guest of this build
   * starts as, and the declaration stays the fallback for a build that serves no
   * defaults at all.
   */
  const [defaults, setDefaults] = useState<VmDefaults | null>(null)
  const [values, setValues] = useState<FormValues>({})
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  // The advanced fields are one folded section: an operator creating their first
  // guest from the template does not have to read the addresses, and one who
  // does can open them.
  const [showAdvanced, setShowAdvanced] = useState(false)
  // Bumped by 重试: both reads are made again, which is the only way out of a
  // failed load while the dialog stays open.
  const [reloadKey, setReloadKey] = useState(0)
  const [placements, setPlacements] = useState<Record<string, Placement>>({})
  const snapshot = useFiles(files ?? null)
  // `null` when this build serves no defaults endpoint, which is not a failure:
  // the fields' declared defaults are then what the form opens with.
  const defaultsUrl = link.maybeUrl('defaults')

  // The declaration and the defaults are read when the form is opened, not when
  // the panel loads: a panel nobody creates from should not ask for either. They
  // are read together, because the form is only usable once both are known, and
  // an in-flight pair is abandoned when the dialog closes or 重试 starts another.
  useEffect(() => {
    if (!open) return
    const controller = new AbortController()
    setSchema(null)
    setDefaults(null)
    void (async () => {
      setError(null)
      try {
        const [declared, published] = await Promise.all([
          api.get<VmSchema>(link.url('schema'), controller.signal),
          defaultsUrl === null
            ? Promise.resolve<VmDefaults | null>(null)
            : api.get<VmDefaults>(defaultsUrl, controller.signal),
        ])
        if (controller.signal.aborted) return
        setSchema(declared)
        setDefaults(published)
        setValues(initialValues(declared, published?.fields ?? null))
      } catch (e: unknown) {
        // An aborted read was replaced by a newer one or by the dialog closing;
        // reporting it would blame the operator for the frontend's own cancel.
        if (!controller.signal.aborted) setError(describeError(e))
      }
    })()
    return () => {
      controller.abort()
    }
  }, [api, link, open, reloadKey, defaultsUrl])

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
    const controller = new AbortController()
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
              controller.signal,
            )
            // A target that names an existing directory is a legitimate
            // destination designation: transfers into it keep the file's own
            // name, and the field value becomes the resulting path.
            const isDirectory = info.directories.some((entry) => entry.path === path)
            const hit = !isDirectory && info.files.some((entry) => entry.path === path)
            next[path] = {
              placed: hit,
              isDirectory,
              detail: hit ? null : isDirectory ? null : `${parts.directory} 里没有这个文件`,
            }
          } catch (e: unknown) {
            next[path] = { placed: false, detail: describeError(e) }
          }
        }
        if (!controller.signal.aborted) setPlacements(next)
      })()
    }, 300)
    return () => {
      clearTimeout(timer)
      controller.abort()
    }
  }, [api, link, schema, values, landed])

  // The gate only applies where the form can read the predicate at all: a build
  // without the filesystem has no listing, and the plane answers at create time.
  const canCheck = link.declared('browse')
  /**
   * Whether the form is ready to be filled and submitted.
   *
   * Both reads have to have arrived: a form built from a declaration but no
   * defaults would open blank where the plane has values, and submitting then
   * would send a request the operator never saw. A build serving no defaults
   * endpoint is ready as soon as the declaration is.
   */
  const ready = schema !== null && (defaultsUrl === null || defaults !== null)
  const unplaced = useMemo(
    () =>
      schema === null || !canCheck
        ? []
        : unplacedFiles(schema, values, (path) => placements[path]?.placed === true),
    [canCheck, placements, schema, values],
  )

  const submit = async () => {
    // One request at a time, including requests whose ID is assigned by the server.
    if (schema === null || !ready || busy || unplaced.length > 0) return
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

  /** One field's row: what it is called, how it is entered, and what it means. */
  const fieldRow = (field: VmSchemaField) => (
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
          <FieldName field={field} />
          <FieldControl
            field={field}
            value={values[field.name] ?? ''}
            onChange={(next) => setValues((current) => ({ ...current, [field.name]: next }))}
          />
        </label>
      )}
      {field.description !== undefined && (
        <p className="text-xs text-muted-foreground">{field.description}</p>
      )}
      {field.example !== undefined && (
        <p className="text-xs text-muted-foreground">
          例：<span className="font-mono">{String(field.example)}</span>
        </p>
      )}
    </div>
  )

  return (
    <Dialog open={open} onOpenChange={(next) => !busy && onOpenChange(next)}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>填表创建客户机</DialogTitle>
          <DialogDescription>
            已为当前机器填好默认值。直接点击「创建」，再启动并打开终端即可使用。
            自定义镜像时可选择已有文件或上传。标 * 的是必填项。
          </DialogDescription>
        </DialogHeader>

        <div className="flex max-h-[55vh] flex-col gap-2 overflow-y-auto pr-1">
          {!ready && (
            <p className="flex items-center gap-2 text-sm text-muted-foreground">
              {error === null ? '正在读取字段集与默认值…' : '字段集或默认值没有读出来，原因见下面的提示。'}
              {error !== null && (
                <Button size="sm" variant="outline" onClick={() => setReloadKey((key) => key + 1)}>
                  重试
                </Button>
              )}
            </p>
          )}
          {ready && schema !== null && basicFields(schema).map((field) => fieldRow(field))}
          {ready && schema !== null && (
            <AdvancedFields
              schema={schema}
              open={showAdvanced}
              onToggle={() => setShowAdvanced((current) => !current)}
              render={fieldRow}
            />
          )}
          {!canCheck && (
            <p className="text-xs text-muted-foreground">
              这个构建没有文件系统能力，表单没法在这里确认文件是否就位；创建时由控制面校验。
            </p>
          )}
          {error && (
            <p className="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm">
              {error}
            </p>
          )}
        </div>

        <DialogFooter>
          <Button variant="outline" disabled={busy} onClick={() => onOpenChange(false)}>
            取消
          </Button>
          <Button
            disabled={busy || !ready || unplaced.length > 0}
            onClick={() => void submit()}
          >
            {busy ? '创建中…' : '创建'}
          </Button>
        </DialogFooter>
        {unplaced.length > 0 && (
          <p className="text-xs text-warn">
            尚未找到以下镜像，请检查路径、选择已有文件或上传后重试：
            {unplaced.join('、')}
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
 * typed or picked and dropping a local file on the field sends it to exactly the
 * path the field names. So "the transfer finished" and "this field is filled with
 * a placed file" are the same event, and 创建 unlocks on the facts rather than on
 * a promise — the same predicate the transfer's placement and the create gate
 * read.
 *
 * The path itself is never invented here. The template's own default prefills the
 * field, its `example` is what the empty control hints at, and a transfer only
 * happens where the value already says. A value the listing does not confirm is
 * reported with the path spelled out and the two ways out — send that file here,
 * or pick a file that is already there — because uploading is one way to satisfy
 * the field, not a requirement of it.
 */
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
  const [pickMode, setPickMode] = useState<'file' | 'directory' | null>(null)
  const [dragOver, setDragOver] = useState(false)
  const [activeId, setActiveId] = useState<string | null>(null)
  const [chosenDir, setChosenDir] = useState<string | null>(null)
  // The last pick this field refused by extension: it is not a transfer, so it
  // lives beside the transfer rows instead of inside them.
  const [refused, setRefused] = useState<string | null>(null)
  const chooser = useRef<HTMLInputElement | null>(null)

  // The hint is the plane's own example, and it is only a hint: an empty field is
  // an empty field, and nothing is adopted as its value behind the operator's back.
  const hint = exampleHint(field)
  const parts = splitGuestPath(value)
  // The field reports the transfer it started itself. Another field runs its
  // own transfers against the same shared service, and a value that happens to
  // name the same path must not inherit someone else's progress.
  const active =
    activeId === null ? undefined : snapshot.transfers.find((row) => row.id === activeId)
  const transferring = active !== undefined && isMoving(active)

  // The transfer destination is a directory, in the order the operator last
  // expressed it: one picked in the browser, then what the value itself says —
  // the value when the listing confirmed it is a directory, otherwise the
  // directory the value names a file in. No value means no destination.
  const destination =
    chosenDir ?? transferDestination(value, status?.isDirectory)

  const uploadTo = (file: File) => {
    if (files === null || transferring || destination === null) return
    if (!acceptMatches(file.name, field.accept)) {
      setRefused(`只接受 ${field.accept?.join(' / ')} 文件，收到的是 ${file.name}`)
      return
    }
    setRefused(null)
    const id = sessionId(file, destination)
    setActiveId(id)
    // The destination keeps the file's own name; the field value becomes the
    // path the placed file will answer to. Joining rather than concatenating is
    // what keeps a root destination honest: `//name` is not a path anything can
    // open, and the form would then never see its own transfer as placed.
    const placed = joinGuestPath(destination, file.name)
    if (value.trim() !== placed) onChange(placed)
    void files.upload(file, destination)
  }

  let state: { text: string; tone: 'placed' | 'pending' | 'failed' }
  if (transferring) {
    const percent = active.total > 0 ? Math.round((active.written / active.total) * 100) : 0
    state = { text: `传输中 ${active.written} / ${active.total} B（${percent}%）`, tone: 'pending' }
  } else if (active?.phase === 'needs-name') {
    // "already exists" is usually a file the operator meant to pick, not to
    // overwrite: both ways out are said, since either clears the field.
    const exists = active.detail?.includes('already exists') ?? false
    state = {
      text: exists
        ? '目标已存在：换个目录再传，或它本身就是你要的文件——用「选择已就位」直接选它'
        : `放置被拒（${active.detail ?? '目标已占用'}），换个目录再传`,
      tone: 'failed',
    }
  } else if (active?.phase === 'failed') {
    state = { text: `传输失败：${active.detail ?? ''}`, tone: 'failed' }
  } else if (refused !== null) {
    state = { text: refused, tone: 'failed' }
  } else if (status?.placed || active?.phase === 'placed') {
    state = { text: '已就位', tone: 'placed' }
  } else if (status?.isDirectory) {
    // A directory value is the destination designation: transfers into it
    // keep the file's own name and the value becomes the resulting path.
    state = {
      text: `目标目录：传进来的文件会放进 ${value.trim()}，并保持它自己的文件名`,
      tone: 'pending',
    }
  } else if (value.trim().length === 0) {
    state = {
      text: '这个字段现在是空的：用「选择已就位…」从 guest 里挑一个文件，或先填一个绝对路径再点「传文件…」。',
      tone: 'pending',
    }
  } else if (parts === null) {
    state = {
      text: `要一个绝对路径（以 / 开头），收到「${value.trim()}」；也可以用「选择已就位…」挑一个。`,
      tone: 'failed',
    }
  } else {
    // The path is spelled out again here: "not in place" is only actionable if
    // the operator can see which path, and the two ways out are named rather
    // than left to be discovered.
    state = {
      text: `尚未就位：${value.trim()}${status?.detail ? `（${status.detail}）` : ''}。可以点「传文件…」把这个文件传到 ${
        destination ?? '它所在的目录'
      }，或者用「选择已就位…」换成 guest 里已有的文件。`,
      tone: 'pending',
    }
  }

  return (
    <div className="flex flex-col gap-1">
      <FieldName field={field} />
      {/* The whole row is the drop target, the way a file manager is: what you
          drop on it goes into the destination directory it resolves to. */}
      <div
        className={cn(
          'flex flex-wrap items-center gap-2 rounded-md border p-2',
          dragOver && 'border-primary bg-primary/10',
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
          aria-label={fieldLabel(field)}
          value={value}
          spellCheck={false}
          className="min-w-56 flex-1 font-mono text-xs"
          placeholder={hint}
          onChange={(event) => {
            // Typing is a new intent: it detaches this field from the transfer
            // it last started and from a destination picked in the browser.
            setActiveId(null)
            setChosenDir(null)
            setRefused(null)
            onChange(event.target.value)
          }}
        />
        {files !== null && (
          <>
            <Button
              size="sm"
              variant="outline"
              disabled={transferring || destination === null}
              title={
                destination === null
                  ? '先填一个绝对路径（或点「选择已就位…」挑一个文件），再传本地文件过去'
                  : `把本地文件传到 ${destination}`
              }
              onClick={() => setPickMode('directory')}
            >
              传文件…
            </Button>
            <input
              ref={chooser}
              type="file"
              className="hidden"
              accept={field.accept?.join(',')}
              onChange={(event) => {
                const chosen = event.target.files?.[0]
                if (chosen !== undefined) uploadTo(chosen)
                event.target.value = ''
              }}
            />
          </>
        )}
        {link.declared('browse') && (
          <Button size="sm" variant="outline" onClick={() => setPickMode('file')}>
            选择已就位…
          </Button>
        )}
      </div>
      <p
        className={cn(
          'text-xs',
          state.tone === 'placed' && 'text-signal',
          state.tone === 'pending' && 'text-muted-foreground',
          state.tone === 'failed' && 'text-alert',
        )}
      >
        {state.text}
      </p>
      {files !== null && link.declared('browse') && (
        <FilePicker
          api={api}
          listing={(path) => `${link.url('browse')}?path=${encodeURIComponent(path)}`}
          makeFolder={(parent, name) => files.mkdir(parent, name)}
          open={pickMode !== null}
          mode={pickMode ?? 'file'}
          onOpenChange={(open) => {
            if (!open) setPickMode(null)
          }}
          onPick={(picked) => {
            if (pickMode === 'directory') {
              // The operator has named where the next file goes; the local
              // file itself is asked for right away, inside this same click.
              setChosenDir(picked)
              setPickMode(null)
              chooser.current?.click()
              return
            }
            setActiveId(null)
            setChosenDir(null)
            setRefused(null)
            onChange(picked)
          }}
          accept={field.accept}
          initial={chosenDir ?? parts?.directory ?? '/'}
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
 * anything. A `boolean` is a choice between 是 and 否, and its text is the only
 * thing that leaves this control: `./schema` is what turns it into the JSON
 * boolean the plane reads. Everything else is text, which is also what a field
 * type this build does not know becomes. A control that shows a hint accepts
 * Enter as "make the hint the value", the way an address bar accepts its own
 * suggestion — the hint is the plane's own example, and typing it by hand is
 * exactly reproducing it.
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
  if (field.type === 'boolean') {
    return (
      <select
        className={SELECT_CLASS}
        value={value}
        onChange={(event) => onChange(event.target.value)}
      >
        {/* Only fields without an initial choice offer an unset value. */}
        {!field.required && (field.default == null || value === '') && <option value="">未设置</option>}
        <option value="true">是</option>
        <option value="false">否</option>
      </select>
    )
  }
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
      placeholder={exampleHint(field)}
      onChange={(event) => onChange(event.target.value)}
      onKeyDown={(event) => {
        if (event.key !== 'Enter' || value.trim().length > 0) return
        const hint = event.currentTarget.placeholder
        if (hint.length > 0) onChange(hint)
      }}
    />
  )
}

/**
 * What a field is called: the plane's label when it declares one, with the
 * identifier beside it so the label and the configuration file agree.
 *
 * The identifier is what a `vm show` or a pasted TOML names the field by, which
 * is exactly why it stays visible — small, and not in the way of the label.
 */
function FieldName({ field }: { field: VmSchemaField }) {
  return (
    <span className="flex flex-wrap items-baseline gap-1.5">
      <span className="text-xs font-medium">{fieldLabel(field)}</span>
      {field.required && <span className="text-xs text-destructive">*</span>}
    </span>
  )
}

/**
 * The fields the declaration marks `advanced`, folded into one section.
 *
 * Folded rather than hidden: they are real fields of the template, so an
 * operator who needs them must be able to reach them — and the summary names
 * what is inside, so reaching them does not require opening the section first to
 * find out whether the field being looked for is in there.
 */
function AdvancedFields({
  schema,
  open,
  onToggle,
  render,
}: {
  schema: VmSchema
  open: boolean
  onToggle(): void
  render(field: VmSchemaField): ReactNode
}) {
  const fields = advancedFields(schema)
  if (fields.length === 0) return null
  return (
    <div className="flex flex-col gap-2 rounded-md border p-2">
      <button
        type="button"
        aria-expanded={open}
        className="flex items-center justify-between gap-2 text-left"
        onClick={onToggle}
      >
        <span className="flex min-w-0 items-center gap-1.5 text-xs font-medium">
          <ChevronRight
            className={cn('h-3.5 w-3.5 shrink-0 transition-transform', open && 'rotate-90')}
          />
          高级设置（{fields.length} 项）
          <span className="truncate text-[10px] font-normal text-muted-foreground">
            {fields.map((field) => fieldLabel(field)).join('、')}
          </span>
        </span>
        <span className="shrink-0 text-[11px] text-muted-foreground">
          {open ? '收起' : '展开'}
        </span>
      </button>
      {open && fields.map((field) => render(field))}
    </div>
  )
}
