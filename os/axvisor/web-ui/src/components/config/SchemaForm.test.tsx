import { renderToStaticMarkup } from 'react-dom/server'
import { describe, expect, it } from 'vitest'
import type { RJSFSchema } from '@rjsf/utils'
import guestSchema from '@/generated/guest-config.schema.json'
import { SchemaForm } from './SchemaForm'

describe('generated schema rendering', () => {
  it('renders every generated section without unsupported schemas or duplicate section headings', () => {
    for (const section of Object.keys(guestSchema.properties)) {
      const html = renderToStaticMarkup(
        <SchemaForm
          schema={guestSchema as RJSFSchema}
          data={{
            base: { id: 7, guest_type: 'virtualized' },
            kernel: { memory_regions: [{ gpa: 4096, size: 8192, flags: 7, map_type: 0 }] },
            devices: { virtual: [{ id: 'a', model: 'example', options: { queue: 4 } }] },
          }}
          activeProperty={section}
          onChange={() => {}}
        />,
      )
      expect(html.match(/Unsupported field schema|Unknown field type|GuestConfig/)).toBeNull()
      expect(html.match(new RegExp(`<legend[^>]*id="root_${section}__title"`))).toBeNull()
      if (section === 'base') {
        for (const variant of guestSchema.definitions.GuestType.oneOf) {
          expect(html.match(new RegExp(`<option[^>]*>${variant.const}</option>`))).not.toBeNull()
        }
        expect(html.match(/id="root_base_guest_type__description"/g)).toHaveLength(1)
        expect(html).toContain('虚拟 CPU 数量（个）')
      }
    }
  })
})
