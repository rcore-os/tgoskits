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
 * A value the plane publishes for this build (`defaults`, from the endpoint of
 * the same name) wins over the field's own declared default, which stays as the
 * fallback for a build that serves no defaults. Either way a filled value is
 * filled in rather than left blank, so what the operator sees is what the
 * request carries. A `null` means the plane has no value here — an id it assigns
 * itself, say — which is an empty control, and the operator may leave it that way.
 */
export function initialValues(
  schema: VmSchema,
  defaults?: Record<string, string | number | boolean | null> | null,
): FormValues {
  const values: FormValues = {}
  for (const field of schema.fields) {
    const published = defaults?.[field.name]
    values[field.name] = controlText(published === undefined ? field.default : published)
  }
  return values
}

/**
 * One declared value as the text a control holds.
 *
 * A boolean reads as `true`/`false`, which is the spelling `fieldValue` turns
 * back into a real JSON boolean, and `null`/`undefined` reads as the empty
 * control that lets the plane decide — not as the words "null" or "undefined".
 */
function controlText(value: string | number | boolean | null | undefined): string {
  if (value === undefined || value === null) return ''
  return String(value)
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
      if (field.required) return { ok: false, error: `「${fieldLabel(field)}」是必填项` }
      continue
    }
    const converted = fieldValue(field, text)
    if ('error' in converted) return { ok: false, error: converted.error }
    fields[field.name] = converted.value
  }
  return { ok: true, fields }
}

/** The name a field is shown under: the plane's label when it declares one. */
export function fieldLabel(field: VmSchemaField): string {
  const label = field.label?.trim() ?? ''
  return label.length > 0 ? label : field.name
}

/**
 * The fields the form shows first, and the ones it folds away.
 *
 * The split is the declaration's to make: how much of a template a first-time
 * operator has to read is a property of the template. Nothing here decides which
 * fields those are, so a field that moves between the two sections — or one the
 * plane adds later — needs no edit in this frontend.
 */
export function basicFields(schema: VmSchema): VmSchemaField[] {
  return schema.fields.filter((field) => field.advanced !== true)
}

/** The fields the form keeps behind 「高级设置」 until the operator asks for them. */
export function advancedFields(schema: VmSchema): VmSchemaField[] {
  return schema.fields.filter((field) => field.advanced === true)
}

/**
 * What a boolean field's text means, or `null` when it is not a boolean.
 *
 * Only the two spellings the form's own control writes are read: the value comes
 * from a choice between 是 and 否, and accepting more spellings would invent a
 * second way to say the same thing.
 */
export function booleanValue(text: string): boolean | null {
  const value = text.trim().toLowerCase()
  if (value === 'true') return true
  if (value === 'false') return false
  return null
}

/**
 * The sample a field's empty control shows: the example the plane declares.
 *
 * A path or address the template expects is the template's fact, so the form
 * shows the one the declaration carries. Spelling one out here would present a
 * single build's answer as the only one, and it would go stale the moment the
 * template changed.
 */
export function exampleHint(field: VmSchemaField): string {
  return field.example === undefined ? '' : String(field.example)
}

/**
 * The directory a transfer into this field would land in, `null` when the value
 * names no place to put a file.
 *
 * A value the listing confirmed as an existing directory *is* the destination:
 * the transfer keeps the file's own name and the field's value becomes the
 * resulting path. Anything else is a path for a file, so its directory is where
 * bytes go. An empty or relative value has no destination at all — inventing one
 * would send the bytes somewhere the operator never named.
 */
export function transferDestination(
  value: string,
  isDirectory: boolean | undefined,
): string | null {
  const path = value.trim()
  if (path.length === 0) return null
  const parts = splitGuestPath(path)
  if (parts === null) return null
  return isDirectory === true ? path : parts.directory
}

/** One value as the field's type means it, or why the text cannot be it. */
function fieldValue(field: VmSchemaField, text: string): FieldValue {
  switch (field.type) {
    case 'integer': {
      if (!/^\d+$/.test(text)) {
        return { error: `「${fieldLabel(field)}」要一个非负整数，收到「${text}」` }
      }
      const value = Number(text)
      return Number.isSafeInteger(value)
        ? { value }
        : { error: `「${fieldLabel(field)}」超出浏览器可精确表示的整数范围` }
    }
    case 'address': {
      const address = addressValue(text)
      return address === null
        ? { error: `「${fieldLabel(field)}」要一个地址（十进制或 0x 开头），收到「${text}」` }
        : { value: address }
    }
    case 'boolean': {
      // A boolean is the one value whose text has to become a *different kind*
      // of JSON value: the plane reads `true`, not `"true"`, so sending the
      // text the control was entered with would be a different request.
      const flag = booleanValue(text)
      return flag === null
        ? { error: `「${fieldLabel(field)}」要选 是 或 否，收到「${text}」` }
        : { value: flag }
    }
    case 'enum': {
      const options = field.options ?? []
      // An enum that declares no options accepts any text: the plane knows which
      // models exist, and refusing here would invent a set of its own.
      return options.length === 0 || options.includes(text)
        ? { value: text }
        : { error: `「${fieldLabel(field)}」只能是 ${options.join(' / ')}，收到「${text}」` }
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
 * The path a transfer into `directory` ends up answering to.
 *
 * The inverse of [`splitGuestPath`], and the reason a form joins rather than
 * concatenates: the root directory already ends in its own separator, so
 * `${directory}/${name}` would name `//name` there. That is not a path anything
 * can open, and the mismatch is invisible — the transfer places the file
 * correctly under `/name` while the field holds `//name`, so the form's own
 * "is it in place" lookup never matches and the file reads as never placed.
 */
export function joinGuestPath(directory: string, name: string): string {
  const base = directory.endsWith('/') ? directory.slice(0, -1) : directory
  return `${base}/${name}`
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

/**
 * Whether a file name satisfies a field's declared extensions.
 *
 * A field without a declaration takes every file. A declared one matches by
 * suffix, case-insensitive, so an extension reads the same with or without its
 * leading dot on the file's side.
 */
export function acceptMatches(name: string, accept: string[] | undefined): boolean {
  if (accept === undefined || accept.length === 0) return true
  const lower = name.toLowerCase()
  return accept.some((extension) => lower.endsWith(extension.toLowerCase()))
}
