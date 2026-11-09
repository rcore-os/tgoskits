import type { RJSFSchema } from '@rjsf/utils'
import { parseVisualDraft, serializeVisualDraft, validateTomlSource } from './config-model'
import { validateSchemaData } from './SchemaForm'

export type EditorMode = 'visual' | 'source'

/** Local editor state. Source validity is independent of JSON representation. */
export interface EditorDraft {
  mode: EditorMode
  source: string
  formData: unknown
  error: string | null
  schemaErrors: string[]
  conversionFailed: boolean
  normalizing: boolean
  valid: boolean
}

export function createEditorDraft(source: string, mode: EditorMode, schema: RJSFSchema): EditorDraft {
  const draft = editEditorSource(source)
  return mode === 'visual' ? changeEditorMode(draft, mode, schema) : draft
}

export function editEditorSource(source: string): EditorDraft {
  const error = validateTomlSource(source)
  return {
    mode: 'source',
    source,
    formData: {},
    error,
    schemaErrors: [],
    conversionFailed: false,
    normalizing: false,
    valid: error === null,
  }
}

export function changeEditorMode(
  draft: EditorDraft,
  mode: EditorMode,
  schema: RJSFSchema,
  formSource = draft.source,
): EditorDraft {
  if (mode === draft.mode) return draft
  if (mode === 'source') {
    // A failed serialization has no source representation. Keep the actual form
    // input rather than silently falling back to an older TOML snapshot.
    if (draft.conversionFailed) return draft
    return editEditorSource(draft.source)
  }
  // The request boundary supplies the server's canonical representation. It is
  // form input only: switching views must not replace the operator's source.
  const parsed = parseVisualDraft(formSource)
  if (!parsed.ok) {
    return { ...draft, error: parsed.error, valid: validateTomlSource(draft.source) === null }
  }
  const schemaErrors = validateSchemaData(schema, parsed.data)
  return {
    ...draft,
    mode,
    formData: parsed.data,
    error: null,
    schemaErrors,
    valid: schemaErrors.length === 0,
  }
}

export function editEditorVisual(draft: EditorDraft, formData: unknown, schema: RJSFSchema): EditorDraft {
  const converted = serializeVisualDraft(formData)
  if (!converted.ok) {
    return {
      ...draft,
      formData,
      error: `${converted.error}；请修正当前输入后再切换视图`,
      conversionFailed: true,
      valid: false,
    }
  }
  const schemaErrors = validateSchemaData(schema, formData)
  return {
    ...draft,
    source: converted.toml,
    formData,
    error: null,
    schemaErrors,
    conversionFailed: false,
    valid: schemaErrors.length === 0,
  }
}
