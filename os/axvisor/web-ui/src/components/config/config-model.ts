import { parse, stringify, TomlDate, type TomlTable } from 'smol-toml'

export type VisualDraft =
  | { ok: true; data: TomlTable }
  | { ok: false; error: string }

export type TomlDraft =
  | { ok: true; toml: string }
  | { ok: false; error: string }

/**
 * Parse source for the visual editor without accepting values that JSON Schema
 * controls cannot round-trip exactly.
 *
 * Source mode still accepts these TOML values. The restriction applies only to
 * entering visual mode, so the source remains untouched and can still be saved.
 */
export function parseVisualDraft(source: string): VisualDraft {
  try {
    const data = parse(source, {
      integersAsBigInt: 'asNeeded',
      unsafeKeyBehaviour: 'throw',
    })
    const unsupported = findUnsupportedVisualValue(data)
    if (unsupported !== null) return { ok: false, error: unsupported }
    return { ok: true, data }
  } catch (error: unknown) {
    return { ok: false, error: tomlError(error) }
  }
}

/** Validate source without converting it to the visual editor's JSON model. */
export function validateTomlSource(source: string): string | null {
  try {
    parse(source, {
      integersAsBigInt: 'asNeeded',
      unsafeKeyBehaviour: 'throw',
    })
    return null
  } catch (error: unknown) {
    return tomlError(error)
  }
}

/**
 * Serialize the complete data returned by RJSF with omitExtraData=false.
 * Do not merge old data back: array positions are not identities, and an absent
 * property (including one behind a schema reference) may be an intentional edit.
 */
export function serializeVisualDraft(changed: unknown): TomlDraft {
  const unsupported = findUnsupportedVisualValue(changed)
  if (unsupported !== null) return { ok: false, error: unsupported }
  try {
    return { ok: true, toml: stringify(changed as TomlTable) }
  } catch (error: unknown) {
    return { ok: false, error: tomlError(error) }
  }
}

/** A filename accepted by the pool's create-new endpoint. */
export function validateConfigName(name: string): string | null {
  const value = name.trim()
  if (value.length === 0) return '请输入配置文件名'
  if (value === '.' || value === '..' || value.includes('/') || value.includes('\\')) {
    return '文件名不能包含路径'
  }
  if (value.startsWith('.')) return '文件名不能以 . 开头'
  if (!value.endsWith('.toml')) return '文件名必须以 .toml 结尾'
  return null
}

function findUnsupportedVisualValue(value: unknown, path = '配置'): string | null {
  if (typeof value === 'bigint') {
    return `${path} 含有超出浏览器安全整数范围的值；请留在 TOML 源码模式编辑`
  }
  if (value instanceof TomlDate || value instanceof Date) {
    return `${path} 含有日期或时间值；可视化模式无法无损表示，请留在 TOML 源码模式编辑`
  }
  if (typeof value === 'number' && Number.isInteger(value) && !Number.isSafeInteger(value)) {
    return `${path} 含有超出浏览器安全整数范围的值；请留在 TOML 源码模式编辑`
  }
  if (Array.isArray(value)) {
    for (let index = 0; index < value.length; index += 1) {
      const found = findUnsupportedVisualValue(value[index], `${path}[${index}]`)
      if (found !== null) return found
    }
  } else if (isRecord(value)) {
    for (const [key, item] of Object.entries(value)) {
      const found = findUnsupportedVisualValue(item, `${path}.${key}`)
      if (found !== null) return found
    }
  }
  return null
}

function tomlError(error: unknown): string {
  if (error instanceof Error) {
    return error.message.replace(/^Invalid TOML document:\s*/i, '').trim()
  }
  return String(error)
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
}
