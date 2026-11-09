import { describe, expect, it } from 'vitest'
import type { RJSFSchema } from '@rjsf/utils'
import { createElement } from 'react'
import { renderToStaticMarkup } from 'react-dom/server'
import guestSchema from '@/generated/guest-config.schema.json'
import { SchemaForm } from './SchemaForm'
import { VisualModeRequest } from './visual-mode-request'
import {
  changeEditorMode,
  createEditorDraft,
  editEditorSource,
  editEditorVisual,
} from './editor-draft'

const schema = guestSchema as RJSFSchema

describe('editor draft state', () => {
  it('uses server canonical form data while preserving the compatible source draft', () => {
    const source = '# legacy layout\n[kernel]\nmemory_regions = [[4096, 8192, 7, 0]]\n'
    const canonical = '[kernel]\n[[kernel.memory_regions]]\ngpa = 4096\nsize = 8192\nflags = 7\nmap_type = 0\n'
    const original = createEditorDraft(source, 'source', schema)
    const draft = changeEditorMode(original, 'visual', schema, canonical)
    expect(draft).toMatchObject({
      mode: 'visual',
      source,
      valid: true,
      schemaErrors: [],
      formData: { kernel: { memory_regions: [{ gpa: 4096, size: 8192, flags: 7, map_type: 0 }] } },
    })
    expect(changeEditorMode(draft, 'source', schema).source).toBe(source)
  })

  it('preserves actual draft text through view switches and a failed parse, then accepts a correction', () => {
    const source = '# operator note\n[base]\nid = 7\nname = "guest"\n'
    let draft = createEditorDraft(source, 'source', schema)
    draft = changeEditorMode(draft, 'visual', schema)
    expect(draft.mode).toBe('visual')
    renderToStaticMarkup(createElement(SchemaForm, {
      schema,
      data: draft.formData,
      activeProperty: 'base',
      onChange: (next) => { draft = editEditorVisual(draft, next, schema) },
    }))
    draft = changeEditorMode(draft, 'source', schema)
    expect(draft.source).toBe(source)
    expect(draft.valid).toBe(true)

    draft = editEditorSource('[base\nid = 8')
    draft = changeEditorMode(draft, 'visual', schema)
    expect(draft).toMatchObject({ mode: 'source', source: '[base\nid = 8', valid: false })
    expect(draft.error).not.toBeNull()
    draft = editEditorSource('[base]\nid = 8\n')
    expect(draft).toMatchObject({ source: '[base]\nid = 8\n', valid: true, error: null })
    draft = changeEditorMode(draft, 'visual', schema)
    expect(draft.formData).toEqual({ base: { id: 8 } })
    expect(draft.valid).toBe(true)
  })

  it.each(['value = 9007199254740993\n', 'released = 1979-05-27\n'])(
    'keeps lossless source submittable after refusing visual mode: %s',
    async (source) => {
      let draft = createEditorDraft(source, 'source', schema)
      await new VisualModeRequest().enter(draft, schema, async () => source, (next) => { draft = next })
      expect(draft).toMatchObject({ mode: 'source', source, valid: true })
      expect(draft.error).not.toBeNull()
      expect(changeEditorMode(draft, 'source', schema).valid).toBe(true)
    },
  )

  it('validates the complete visual draft, not the most recently displayed section, and clears stale source errors', () => {
    let draft = createEditorDraft('[base]\nid = 7\n', 'visual', schema)
    draft = editEditorVisual(draft, { base: { id: -1 }, devices: { virtual: [{ id: 'a' }] } }, schema)
    expect(draft.valid).toBe(false)
    expect(draft.schemaErrors.length).toBeGreaterThan(0)
    // Fixing the base section cannot hide the missing model in devices.
    draft = editEditorVisual(draft, { base: { id: 7 }, devices: { virtual: [{ id: 'a' }] } }, schema)
    expect(draft.valid).toBe(false)
    expect(draft.schemaErrors.join('\n')).toContain('model')
    expect(draft.source).toContain('id = "a"')
    draft = changeEditorMode(draft, 'source', schema)
    expect(draft.valid).toBe(true)
    expect(draft.schemaErrors).toEqual([])
    draft = editEditorSource('[base]\nid = 9\n')
    draft = changeEditorMode(draft, 'visual', schema)
    expect(draft.valid).toBe(true)
    expect(draft.schemaErrors).toEqual([])
  })

  it('retains unconvertible form input instead of switching to an older source snapshot', () => {
    let draft = createEditorDraft('[base]\nid = 7\n', 'visual', schema)
    const formData = { base: { id: 9007199254740993n } }
    draft = editEditorVisual(draft, formData, schema)
    expect(draft.valid).toBe(false)
    expect(draft.formData).toEqual(formData)
    draft = changeEditorMode(draft, 'source', schema)
    expect(draft.mode).toBe('visual')
    expect(draft.formData).toEqual(formData)
    draft = editEditorVisual(draft, { base: { id: 8 } }, schema)
    expect(draft.valid).toBe(true)
    expect(changeEditorMode(draft, 'source', schema).source).toContain('id = 8')
  })
})
