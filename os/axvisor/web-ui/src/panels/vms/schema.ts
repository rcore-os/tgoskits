//! The creation form's model: what the declared fields start as, and what a
//! filled form becomes.
//!
//! Both directions are pure, so each can be reasoned about and tested without a
//! browser. `initialValues` turns the declared field set into what the form
//! opens with, and `creationBody` turns the operator's text back into the
//! request body — which is where a form's one kind of value (text) has to become
//! what each field actually means.

import type { VmSchema, VmSchemaField } from '@/api/types'

/** Values as the form holds them: every control yields text. */
export type FormValues = Record<string, string>

/** A request body, or why the form cannot produce one. */
export type CreationBody =
  | { ok: true; fields: Record<string, unknown> }
  | { ok: false; error: string }

/** One converted value, or why the text cannot be what the field means. */
type FieldValue = { value: unknown } | { error: string }

/**
 * What the form opens with.
 *
 * A declared default is filled in rather than left blank, so what the operator
 * sees is what the request carries: a field the template would have filled must
 * not differ from the one on screen. A `null` default means the template has
 * nothing to fill in, which is an empty control.
 */
export function initialValues(schema: VmSchema): FormValues {
  const values: FormValues = {}
  for (const field of schema.fields) {
    values[field.name] =
      field.default === undefined || field.default === null ? '' : String(field.default)
  }
  return values
}

/**
 * The `fields` body of one creation request.
 *
 * A form carries text, so each value is converted where the schema says what it
 * means. An optional field left empty is omitted rather than sent as an empty
 * string, which is what lets the template fill it; a required one left empty is
 * refused here, naming the field, instead of costing a round trip. Nothing is
 * checked beyond the field's own meaning: whether the value makes sense is the
 * plane's answer to give, and it is the one that has to be right.
 */
export function creationBody(schema: VmSchema, values: FormValues): CreationBody {
  const fields: Record<string, unknown> = {}
  for (const field of schema.fields) {
    const text = (values[field.name] ?? '').trim()
    if (text.length === 0) {
      if (field.required) return { ok: false, error: `「${field.name}」是必填项` }
      continue
    }
    const converted = fieldValue(field, text)
    if ('error' in converted) return { ok: false, error: converted.error }
    fields[field.name] = converted.value
  }
  return { ok: true, fields }
}

/** One value as the field's type means it, or why the text cannot be it. */
function fieldValue(field: VmSchemaField, text: string): FieldValue {
  switch (field.type) {
    case 'integer':
      return /^\d+$/.test(text)
        ? { value: Number(text) }
        : { error: `「${field.name}」要一个非负整数，收到「${text}」` }
    case 'address': {
      const address = addressValue(text)
      return address === null
        ? { error: `「${field.name}」要一个地址（十进制或 0x 开头），收到「${text}」` }
        : { value: address }
    }
    case 'enum': {
      const options = field.options ?? []
      // An enum that declares no options accepts any text: the plane knows which
      // models exist, and refusing here would invent a set of its own.
      return options.length === 0 || options.includes(text)
        ? { value: text }
        : { error: `「${field.name}」只能是 ${options.join(' / ')}，收到「${text}」` }
    }
    default:
      // Anything else is carried as the text it was typed as: a field type this
      // build does not know is still a field the plane declared, so the plane
      // decides whether the value is one it accepts.
      return { value: text }
  }
}

/**
 * One address written the way a guest configuration writes it, or `null`.
 *
 * `0x…` is accepted, with or without `_` separators (`0x8020_0000`), because that
 * is the spelling an operator copies out of a config; a decimal number is
 * accepted too. A value a JSON number cannot hold exactly is returned as the
 * text it was typed as: the plane parses that same spelling, while a rounded
 * number would name a different address.
 */
export function addressValue(text: string): number | string | null {
  const compact = text.replace(/_/g, '')
  const hex = /^0[xX][0-9a-fA-F]+$/.test(compact)
  if (!hex && !/^\d+$/.test(compact)) return null
  const value = Number.parseInt(hex ? compact.slice(2) : compact, hex ? 16 : 10)
  return Number.isSafeInteger(value) && value >= 0 ? value : text.trim()
}

/** The fields whose value names a file in the guest filesystem. */
export function fileFields(schema: VmSchema): VmSchemaField[] {
  return schema.fields.filter((field) => field.type === 'file')
}

/**
 * The directory and the final name one guest path is made of, or `null`.
 *
 * A `file` field names the whole path, so a transfer into it has to open in the
 * directory that path lives in and place its bytes under the last component.
 * Anything that is not an absolute path with a file name cannot be a transfer
 * target, and the form says so rather than guessing one.
 */
export function splitGuestPath(path: string): { directory: string; name: string } | null {
  const value = path.trim()
  if (!value.startsWith('/') || value.endsWith('/')) return null
  const separator = value.lastIndexOf('/')
  const name = value.slice(separator + 1)
  if (name.length === 0) return null
  return { directory: separator === 0 ? '/' : value.slice(0, separator), name }
}

/**
 * The guest paths a submit would reference that are not known to be in place.
 *
 * `inPlace` is the predicate the transfer's placement and the create gate read —
 * "this path exists in the guest filesystem" — so the disabled button is that
 * predicate rendered, not a rule of the form's own. Anything unconfirmed counts:
 * a path nobody has looked up is not evidence of a placed file.
 */
export function unplacedFiles(
  schema: VmSchema,
  values: FormValues,
  inPlace: (path: string) => boolean,
): string[] {
  const missing: string[] = []
  for (const field of fileFields(schema)) {
    const path = (values[field.name] ?? '').trim()
    if (path.length > 0 && !inPlace(path)) missing.push(path)
  }
  return missing
}
