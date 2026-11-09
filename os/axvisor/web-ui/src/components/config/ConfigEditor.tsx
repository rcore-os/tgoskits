import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react'
import { Braces, Code2, Info, Loader2 } from 'lucide-react'
import type { RJSFSchema } from '@rjsf/utils'
import { Button } from '@/components/ui/button'
import { cn } from '@/lib/utils'
import { SchemaForm } from './SchemaForm'
import {
  changeEditorMode,
  createEditorDraft,
  editEditorSource,
  editEditorVisual,
  type EditorMode,
} from './editor-draft'
import { VisualModeRequest, type NormalizeConfig } from './visual-mode-request'

export type { EditorMode } from './editor-draft'

export interface ConfigEditorProps {
  schema: RJSFSchema
  initialToml: string
  readOnly?: boolean
  initialMode?: EditorMode
  active?: boolean
  normalize?: NormalizeConfig
  onChange(toml: string, dirty: boolean): void
  onValidityChange?(valid: boolean): void
}

/**
 * One TOML draft with two views.
 *
 * Merely switching views never serializes. The source is normalized only after
 * a visual control changes, at which point the format/comment warning is already
 * visible above the form.
 */
export function ConfigEditor({
  schema,
  initialToml,
  readOnly = false,
  initialMode = 'visual',
  active = true,
  normalize,
  onChange,
  onValidityChange,
}: ConfigEditorProps) {
  const [draft, setDraft] = useState(() => createEditorDraft(
    initialToml,
    initialToml.trim().length > 0 ? 'source' : initialMode,
    schema,
  ))
  const [visualRequests] = useState(() => new VisualModeRequest())
  const normalizeRef = useRef(normalize)
  normalizeRef.current = normalize
  const { mode, source, formData, error, schemaErrors, normalizing } = draft
  const sections = useMemo(() => schemaSections(schema), [schema])
  const [section, setSection] = useState<string | null>(sections[0]?.key ?? null)

  useLayoutEffect(() => {
    visualRequests.cancel()
    const fresh = createEditorDraft(initialToml, initialToml.trim().length > 0 ? 'source' : initialMode, schema)
    setDraft(fresh)
    setSection(schemaSections(schema)[0]?.key ?? null)
    if (active && initialMode === 'visual') {
      void visualRequests.enter(fresh, schema, normalizeRef.current, setDraft)
    }
    return () => visualRequests.cancel()
  }, [active, initialMode, initialToml, schema, visualRequests])

  useEffect(() => {
    onValidityChange?.(draft.valid)
  }, [draft.valid, onValidityChange])

  const switchMode = (next: EditorMode) => {
    if (!active) return
    if (next === 'visual') {
      void visualRequests.enter(draft, schema, normalize, setDraft)
    } else {
      visualRequests.cancel()
      setDraft(changeEditorMode({ ...draft, normalizing: false }, next, schema))
    }
  }

  const visualChange = (next: unknown) => {
    visualRequests.cancel()
    const updated = editEditorVisual(draft, next, schema)
    setDraft(updated)
    onChange(updated.source, updated.conversionFailed || updated.source !== initialToml)
  }

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="flex flex-wrap items-center justify-between gap-2 border-b px-4 py-2.5">
        <div className="flex rounded-md border bg-muted/40 p-0.5" aria-label="编辑视图">
          <ViewButton
            active={mode === 'visual'}
            icon={normalizing ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <Braces className="h-3.5 w-3.5" />}
            onClick={() => switchMode('visual')}
          >
            {normalizing ? '转换中…' : '可视化'}
          </ViewButton>
          <ViewButton
            active={mode === 'source'}
            icon={<Code2 className="h-3.5 w-3.5" />}
            onClick={() => switchMode('source')}
          >
            TOML 源码
          </ViewButton>
        </div>
        {readOnly && (
          <span className="rounded-full border px-2 py-0.5 text-xs text-muted-foreground">只读查看</span>
        )}
      </div>

      {normalizing && (
        <p role="status" className="mx-4 mt-3 text-sm text-muted-foreground">
          正在通过后端解析配置；源码不会改写，仍可继续编辑。
        </p>
      )}
      {error !== null && (
        <p role="alert" className="mx-4 mt-3 rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm">
          {error}
        </p>
      )}
      {mode === 'visual' && schemaErrors.length > 0 && (
        <p role="alert" className="mx-4 mt-3 rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm">
          配置未通过 Schema 校验：{schemaErrors[0]}
        </p>
      )}

      {mode === 'source' ? (
        <div className="flex min-h-0 flex-1 flex-col p-4">
          <label htmlFor="config-toml" className="mb-1.5 text-sm font-medium">
            客户机配置 TOML
          </label>
          <textarea
            id="config-toml"
            className="min-h-[22rem] flex-1 resize-none rounded-md border bg-muted/20 p-3 font-mono text-[13px] leading-6 outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 disabled:cursor-not-allowed disabled:opacity-75"
            value={source}
            readOnly={readOnly}
            spellCheck={false}
            onChange={(event) => {
              const next = event.target.value
              visualRequests.cancel()
              setDraft(editEditorSource(next))
              onChange(next, next !== initialToml)
            }}
          />
        </div>
      ) : (
        <div className="grid min-h-0 flex-1 md:grid-cols-[12rem_minmax(0,1fr)]">
          <nav className="border-b bg-muted/20 p-2 md:border-b-0 md:border-r" aria-label="配置分区">
            <div className="flex gap-1 overflow-x-auto md:flex-col">
              {sections.map((item) => (
                <button
                  key={item.key}
                  type="button"
                  className={cn(
                    'whitespace-nowrap rounded-md px-3 py-2 text-left text-sm outline-none focus-visible:ring-2 focus-visible:ring-ring',
                    section === item.key
                      ? 'bg-background font-medium text-foreground shadow-sm'
                      : 'text-muted-foreground hover:bg-accent/60 hover:text-foreground',
                  )}
                  onClick={() => setSection(item.key)}
                >
                  {item.title}
                </button>
              ))}
            </div>
          </nav>
          <div className="min-h-0 overflow-y-auto px-4 py-4 md:px-6">
            {!readOnly && (
              <p className="mb-4 flex max-w-3xl items-start gap-2 rounded-md border bg-muted/35 px-3 py-2 text-xs text-muted-foreground">
                <Info className="mt-0.5 h-3.5 w-3.5 shrink-0" />
                可视化修改会重新排版 TOML 并移除注释；只切换视图不改写原文。
              </p>
            )}
            {section === 'devices' && (
              <p className="mb-4 max-w-3xl rounded-md border border-warn/35 bg-warn/10 px-3 py-2 text-xs text-muted-foreground">
                设备型号的私有 options 会原样保留，但完整结构不由通用 Schema 描述。需要新增或修改复杂
                options 时，请切换到 TOML 源码。
              </p>
            )}
            <SchemaForm
              schema={schema}
              data={formData}
              activeProperty={section}
              disabled={readOnly}
              onChange={visualChange}
            />
          </div>
        </div>
      )}
    </div>
  )
}

function ViewButton({
  active,
  icon,
  children,
  onClick,
}: {
  active: boolean
  icon: React.ReactNode
  children: React.ReactNode
  onClick(): void
}) {
  return (
    <Button
      type="button"
      size="sm"
      variant="ghost"
      className={cn('h-8 gap-1.5 px-2.5', active && 'bg-background shadow-sm hover:bg-background')}
      aria-pressed={active}
      onClick={onClick}
    >
      {icon}
      {children}
    </Button>
  )
}

function schemaSections(schema: RJSFSchema): { key: string; title: string }[] {
  const properties = schema.properties ?? {}
  return Object.entries(properties).map(([key, value]) => ({
    key,
    title: sectionTitle(
      key,
      value !== null && typeof value === 'object' && typeof value.title === 'string'
        ? value.title
        : undefined,
    ),
  }))
}

function sectionTitle(key: string, declared: string | undefined): string {
  if (key === 'base') return '身份与资源'
  if (key === 'kernel') return '启动'
  if (key === 'devices') return '设备'
  return declared ?? humanize(key)
}

function humanize(value: string): string {
  return value
    .replace(/[_-]+/g, ' ')
    .replace(/\b\w/g, (letter) => letter.toUpperCase())
}
