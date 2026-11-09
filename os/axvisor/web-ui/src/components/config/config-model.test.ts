import { describe, expect, it } from 'vitest'
import type { RJSFSchema } from '@rjsf/utils'
import Form from '@rjsf/core/lib/components/Form.js'
import guestSchema from '@/generated/guest-config.schema.json'
import { SchemaForm } from './SchemaForm'
import {
  parseVisualDraft,
  serializeVisualDraft,
  validateConfigName,
  validateTomlSource,
} from './config-model'

const SCHEMA: RJSFSchema = {
  type: 'object',
  properties: {
    base: {
      type: 'object',
      properties: {
        id: { type: 'integer' },
        name: { type: 'string' },
      },
    },
    devices: {
      type: 'array',
      items: {
        type: 'object',
        properties: {
          id: { type: 'string' },
          model: { type: 'string' },
          options: { type: 'object', additionalProperties: true },
        },
      },
    },
  },
}

// Drive the actual RJSF change pipeline in-process, without a DOM renderer. Only
// React's commit scheduling is synchronous here; schema/data handling is real.
function editForm(schema: RJSFSchema, data: unknown, property: string, next: unknown, path = property): unknown {
  let changed = data
  const element = SchemaForm({ schema, data, activeProperty: property, onChange: (value) => { changed = value } })
  const form = new Form(element.props)
  form.setState = (update, callback) => {
    const patch = typeof update === 'function' ? update(form.state, form.props) : update
    form.state = { ...form.state, ...patch }
    callback?.()
  }
  form.setFieldValue(path, next)
  return changed
}

describe('configuration draft conversion', () => {
  it('normalizes a visual edit without losing nested unknown and open device options', () => {
    const source = [
      '[base]',
      'id = 7',
      'name = "guest"',
      'future = "preserve-me"',
      '',
      '[[devices]]',
      'model = "virtio-blk"',
      'options = { queue = 4, vendor_flag = true }',
      '',
    ].join('\n')
    const parsed = parseVisualDraft(source)
    expect(parsed.ok).toBe(true)
    if (!parsed.ok) return

    const result = serializeVisualDraft(editForm(SCHEMA, parsed.data, 'base', 'edited', 'base.name'))
    expect(result.ok).toBe(true)
    if (!result.ok) return
    const reparsed = parseVisualDraft(result.toml)
    expect(reparsed).toMatchObject({
      ok: true,
      data: {
        base: { id: 7, name: 'edited', future: 'preserve-me' },
        devices: [
          {
            model: 'virtio-blk',
            options: { queue: 4, vendor_flag: true },
          },
        ],
      },
    })
  })

  it('rejects invalid TOML with a diagnostic', () => {
    const source = '[base\nid = 7'
    const result = parseVisualDraft(source)
    expect(result.ok).toBe(false)
    expect(result.ok === false && result.error.length).toBeGreaterThan(0)
  })

  it('accepts lossless source but blocks unsafe integers and dates from visual editing', () => {
    const large = 'value = 9007199254740993\n'
    expect(validateTomlSource(large)).toBeNull()
    expect(parseVisualDraft(large)).toMatchObject({
      ok: false,
      error: expect.stringContaining('安全整数'),
    })

    const dated = 'released = 1979-05-27\n'
    expect(validateTomlSource(dated)).toBeNull()
    expect(parseVisualDraft(dated)).toMatchObject({
      ok: false,
      error: expect.stringContaining('日期或时间'),
    })
  })
})

describe('pool filename validation', () => {
  it('accepts one visible TOML filename and rejects paths or ambiguous names', () => {
    expect(validateConfigName('guest.toml')).toBeNull()
    for (const invalid of ['', '../guest.toml', '/guest.toml', '.guest.toml', 'guest.conf']) {
      expect(validateConfigName(invalid)).not.toBeNull()
    }
  })
})

describe('visual edits round-trip without resurrecting data', () => {
  it.each(['delete first', 'reorder'])('keeps open options with their own array item on %s', (operation) => {
    const parsed = parseVisualDraft(`
[[devices]]
id = "a"
model = "example"
options = { alpha = 1 }
[[devices]]
id = "b"
model = "example"
options = { beta = 2 }
`)
    if (!parsed.ok) throw new Error(parsed.error)
    const changed = structuredClone(parsed.data) as Record<string, unknown>
    const devices = changed.devices as Record<string, unknown>[]
    changed.devices = operation === 'delete first' ? devices.slice(1) : [...devices].reverse()
    const result = serializeVisualDraft(editForm(SCHEMA, parsed.data, 'devices', changed.devices))
    if (!result.ok) throw new Error(result.error)
    expect(parseVisualDraft(result.toml)).toEqual({ ok: true, data: changed })
  })

  it.each(['delete first', 'reorder'])('respects %s and known-field deletion behind generated allOf and item $ref schemas', (operation) => {
    const parsed = parseVisualDraft(`
[base]
id = 7
phys_cpu_ids = [1, 2]
future = "retained"
[[devices.virtual]]
id = "a"
model = "example"
options = { alpha = 1 }
[[devices.virtual]]
id = "b"
model = "example"
options = { beta = 2 }
`)
    if (!parsed.ok) throw new Error(parsed.error)
    const changed = structuredClone(parsed.data)
    delete (changed.base as Record<string, unknown>).phys_cpu_ids
    const devices = changed.devices as { virtual: Record<string, unknown>[] }
    devices.virtual = operation === 'delete first' ? devices.virtual.slice(1) : devices.virtual.reverse()
    delete devices.virtual[0].model
    const baseEdited = editForm(guestSchema as RJSFSchema, parsed.data, 'base', undefined, 'base.phys_cpu_ids')
    const result = serializeVisualDraft(editForm(guestSchema as RJSFSchema, baseEdited, 'devices', devices.virtual, 'devices.virtual'))
    if (!result.ok) throw new Error(result.error)
    expect(parseVisualDraft(result.toml)).toEqual({ ok: true, data: changed })
  })
})
