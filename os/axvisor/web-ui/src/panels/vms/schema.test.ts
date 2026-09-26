//! The creation form's model: the two conversions between the declared field set
//! and one request body.
//!
//! What these tests hold is that the form follows the schema rather than a field
//! list of its own: a field added to the declaration appears in the body, and one
//! removed stops appearing, without an edit here.

import { describe, expect, it } from 'vitest'
import type { VmSchema } from '@/api/types'
import {
  addressValue,
  creationBody,
  fileFields,
  initialValues,
  splitGuestPath,
  unplacedFiles,
} from './schema'

/** The declaration the control plane currently serves, as a fixture. */
const SCHEMA: VmSchema = {
  fields: [
    { name: 'id', type: 'integer', required: true },
    { name: 'name', type: 'string', required: true },
    { name: 'kernel_path', type: 'file', required: true },
    { name: 'image_location', type: 'enum', required: false, default: 'fs', options: ['fs'] },
    { name: 'entry_point', type: 'address', required: true },
    { name: 'kernel_load_addr', type: 'address', required: true },
    { name: 'memory_base', type: 'address', required: true },
    { name: 'memory_mb', type: 'integer', required: true },
    { name: 'guest_type', type: 'enum', required: false, default: 'virtualized', options: ['virtualized', 'passthrough'] },
    { name: 'cpu_num', type: 'integer', required: false, default: 1 },
    { name: 'cmdline', type: 'string', required: false, default: null },
  ],
}

const FILLED = {
  id: '7',
  name: 'probe-guest',
  kernel_path: '/guest/linux/linux-qemu',
  image_location: 'fs',
  entry_point: '0x8020_0000',
  kernel_load_addr: '0x8020_0000',
  memory_base: '0x8000_0000',
  memory_mb: '256',
  guest_type: 'virtualized',
  cpu_num: '1',
  cmdline: '',
}

describe('initialValues', () => {
  it('opens with the defaults the template fills', () => {
    const values = initialValues(SCHEMA)
    expect(values.guest_type).toBe('virtualized')
    expect(values.cpu_num).toBe('1')
    // The only kernel source a form-made guest has is the filesystem, and the
    // declaration says so with a default rather than a required choice.
    expect(values.image_location).toBe('fs')
  })

  it('leaves a field the template has nothing for empty', () => {
    // `null` is the declaration's way of saying the template fills nothing, and
    // an empty control is what lets the plane apply its own default.
    expect(initialValues(SCHEMA).cmdline).toBe('')
    expect(initialValues(SCHEMA).id).toBe('')
  })
})

describe('creationBody', () => {
  it('converts every declared field of a filled form', () => {
    const body = creationBody(SCHEMA, FILLED)
    expect(body).toEqual({
      ok: true,
      fields: {
        id: 7,
        name: 'probe-guest',
        kernel_path: '/guest/linux/linux-qemu',
        image_location: 'fs',
        entry_point: 0x80200000,
        kernel_load_addr: 0x80200000,
        memory_base: 0x80000000,
        memory_mb: 256,
        guest_type: 'virtualized',
        cpu_num: 1,
      },
    })
  })

  it('omits an optional field left empty instead of sending an empty string', () => {
    // Sending `""` would be a value the plane has to reject; omitting it is what
    // lets the template fill the field.
    const body = creationBody(SCHEMA, { ...FILLED, cmdline: '' })
    expect(body.ok && 'cmdline' in body.fields).toBe(false)
  })

  it('carries a filled optional field', () => {
    const body = creationBody(SCHEMA, { ...FILLED, cmdline: 'console=ttyAMA0' })
    expect(body.ok && body.fields.cmdline).toBe('console=ttyAMA0')
  })

  it('names the field when a required one is empty', () => {
    const body = creationBody(SCHEMA, { ...FILLED, name: '   ' })
    expect(body).toEqual({ ok: false, error: '「name」是必填项' })
  })

  it('refuses text that is not an integer', () => {
    for (const value of ['1.5', 'two', '-1']) {
      const body = creationBody(SCHEMA, { ...FILLED, cpu_num: value })
      expect(body.ok).toBe(false)
      expect(body.ok === false && body.error).toContain('cpu_num')
    }
  })

  it('accepts an address in either spelling', () => {
    const decimal = creationBody(SCHEMA, { ...FILLED, entry_point: '2149580800' })
    expect(decimal.ok && decimal.fields.entry_point).toBe(0x80200000)
    const hex = creationBody(SCHEMA, { ...FILLED, kernel_load_addr: '0x80200000' })
    expect(hex.ok && hex.fields.kernel_load_addr).toBe(0x80200000)
  })

  it('refuses an enum value the declaration does not offer', () => {
    const body = creationBody(SCHEMA, { ...FILLED, image_location: 'network' })
    expect(body.ok).toBe(false)
    expect(body.ok === false && body.error).toContain('fs')
  })

  it('follows the declaration rather than a field list of its own', () => {
    // A field the plane adds appears in the body with no edit to this form's
    // code, which is the property the whole descriptor exists for.
    const extended: VmSchema = {
      fields: [...SCHEMA.fields, { name: 'dtb_path', type: 'string', required: false }],
    }
    const body = creationBody(extended, { ...FILLED, dtb_path: '/guest/guest.dtb' })
    expect(body.ok && body.fields.dtb_path).toBe('/guest/guest.dtb')

    const reduced: VmSchema = {
      fields: SCHEMA.fields.filter((field) => field.name !== 'cpu_num'),
    }
    const smaller = creationBody(reduced, FILLED)
    expect(smaller.ok && 'cpu_num' in smaller.fields).toBe(false)
  })
})

describe('addressValue', () => {
  it('reads the separators a guest configuration uses', () => {
    expect(addressValue('0x8020_0000')).toBe(0x80200000)
  })

  it('refuses text that is no address', () => {
    expect(addressValue('0x8020000g')).toBeNull()
    expect(addressValue('')).toBeNull()
    expect(addressValue('-0x10')).toBeNull()
  })

  it('keeps a value a number cannot hold exactly as the text it was typed as', () => {
    // Rounding would name a different address; the plane parses this spelling.
    expect(addressValue('0xffff_ffff_ffff_ffff')).toBe('0xffff_ffff_ffff_ffff')
  })
})

describe('fileFields', () => {
  it('picks the fields the declaration marks as guest files', () => {
    // The form must not guess from a name: which fields are files is the
    // declaration's to say, so a path field the plane adds later is followed.
    expect(fileFields(SCHEMA).map((field) => field.name)).toEqual(['kernel_path'])
  })
})

describe('splitGuestPath', () => {
  it('splits a guest path into the directory a transfer opens in and the final name', () => {
    expect(splitGuestPath('/guest/linux/linux-qemu')).toEqual({
      directory: '/guest/linux',
      name: 'linux-qemu',
    })
    expect(splitGuestPath('/linux-qemu')).toEqual({ directory: '/', name: 'linux-qemu' })
  })

  it('refuses anything that cannot be a transfer target', () => {
    // A transfer has to open somewhere and place under one name; a relative
    // path, a directory or an empty one names neither.
    expect(splitGuestPath('guest/linux-qemu')).toBeNull()
    expect(splitGuestPath('/guest/linux/')).toBeNull()
    expect(splitGuestPath('/')).toBeNull()
    expect(splitGuestPath('')).toBeNull()
  })
})

describe('unplacedFiles', () => {
  it('reports a referenced path the predicate does not confirm', () => {
    const missing = unplacedFiles(SCHEMA, FILLED, (path) => path !== '/guest/linux/linux-qemu')
    expect(missing).toEqual(['/guest/linux/linux-qemu'])
  })

  it('passes a form whose referenced files are all confirmed', () => {
    expect(unplacedFiles(SCHEMA, FILLED, () => true)).toEqual([])
  })

  it('ignores a file field left empty, which the template or the plane handles', () => {
    const optional: VmSchema = {
      fields: [{ name: 'dtb_path', type: 'file', required: false }],
    }
    expect(unplacedFiles(optional, { dtb_path: '' }, () => false)).toEqual([])
  })

  it('counts a path nobody has looked up as unplaced', () => {
    // "Unknown" is not "placed": the button stays dark until a listing confirms
    // the file, which is what keeps a stale lookup from unlocking a submit.
    expect(unplacedFiles(SCHEMA, FILLED, () => false)).toEqual(['/guest/linux/linux-qemu'])
  })
})
