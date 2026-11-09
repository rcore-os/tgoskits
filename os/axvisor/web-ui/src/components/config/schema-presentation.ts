import type { RJSFSchema } from '@rjsf/utils'

// Presentation only: types, constraints, defaults and option values stay with
// the generated schema. Unrecognized fields keep their declared labels.
const LABELS: Record<string, string> = {
  id: '标识',
  name: '名称',
  cpu_num: '虚拟 CPU 数量（个）',
  guest_type: '客户机类型',
  phys_cpu_ids: '物理 CPU 标识列表',
  phys_cpu_sets: 'CPU 亲和性掩码列表',
  kernel_path: '内核路径',
  kernel_load_addr: '内核加载地址（字节）',
  entry_point: '入口地址（字节）',
  cmdline: '内核启动参数',
  boot_protocol: '启动协议',
  memory_regions: '内存区域',
  base: '起始地址（字节）',
  size: '大小（字节）',
  gpa: '客户机物理地址（字节）',
  flags: '属性标志',
  dtb_path: '设备树路径',
  dtb_load_addr: '设备树加载地址（字节）',
  ramdisk_path: '初始内存盘路径',
  ramdisk_load_addr: '初始内存盘加载地址（字节）',
  enable_bios: '启用固件',
  bios_path: '固件路径',
  bios_load_addr: '固件加载地址（字节）',
  uefi_firmware_path: 'UEFI 固件路径',
  passthrough: '直通设备',
  disabled: '排除的物理设备',
  virtual: '虚拟设备',
  path: '设备路径',
  model: '设备型号',
}

/** Adapt JSON Schema syntax for rendering, without changing its source. */
export function prepareFormSchema(schema: RJSFSchema): RJSFSchema {
  const result = { ...schema }
  for (const keyword of ['properties', 'definitions', '$defs', 'patternProperties'] as const) {
    const entries = schema[keyword]
    if (entries !== undefined && typeof entries === 'object') {
      result[keyword] = Object.fromEntries(Object.entries(entries).map(([name, value]) => {
        if (typeof value !== 'object' || value === null) return [name, value]
        const adapted = prepareFormSchema(value as RJSFSchema)
        if (keyword === 'properties' && LABELS[name] !== undefined) adapted.title = LABELS[name]
        return [name, adapted]
      }))
    }
  }
  for (const keyword of ['allOf', 'anyOf', 'oneOf'] as const) {
    if (schema[keyword]) result[keyword] = schema[keyword].map((item) => prepareFormSchema(item as RJSFSchema))
  }
  if (Array.isArray(schema.items)) {
    result.items = schema.items.map((item) => prepareFormSchema(item as RJSFSchema))
  } else if (typeof schema.items === 'object') {
    result.items = prepareFormSchema(schema.items as RJSFSchema)
  }
  for (const keyword of ['additionalProperties', 'not', 'if', 'then', 'else'] as const) {
    if (typeof schema[keyword] === 'object') result[keyword] = prepareFormSchema(schema[keyword] as RJSFSchema)
  }

  // RJSF cannot infer the field type of Schemars' const-only unions. Adding the
  // shared primitive type and redundant enum lets RJSF render its normal
  // selector, including when the union is nested inside allOf/$ref. Keep the
  // original union constraints; values still come entirely from the schema.
  const variants = result.oneOf ?? result.anyOf
  if (result.type === undefined && variants?.length) {
    const first = variants[0] as RJSFSchema
    const type = first.type
    const values = variants.flatMap((variant) =>
      typeof variant === 'object' && variant.const !== undefined ? [variant.const] : [],
    )
    if (
      typeof type === 'string' &&
      ['string', 'integer', 'number', 'boolean'].includes(type) &&
      values.length === variants.length &&
      variants.every((variant) =>
        typeof variant === 'object' && variant.type === type && Object.prototype.hasOwnProperty.call(variant, 'const'),
      )
    ) {
      result.type = type
      result.enum = values
    }
  }
  return result
}
