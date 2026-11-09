// Import the public Form subpath rather than the package barrel. The barrel also
// exports `getTestRegistry`, whose test-only AJV peer must not enter this CSP
// constrained production bundle.
import Form from '@rjsf/core/lib/components/Form.js'
import { customizeValidator } from '@rjsf/validator-cfworker'
import { buttonId, canExpand, getTemplate, getUiOptions } from '@rjsf/utils'
import { Info } from 'lucide-react'
import type {
  DescriptionFieldProps,
  FieldTemplateProps,
  ObjectFieldTemplateProps,
  RJSFSchema,
  RJSFValidationError,
  UiSchema,
} from '@rjsf/utils'
import { prepareFormSchema } from './schema-presentation'

const validator = customizeValidator({ draft: '7', shortCircuit: false })

export interface SchemaFormProps {
  schema: RJSFSchema
  data: unknown
  disabled?: boolean
  activeProperty?: string | null
  onChange(data: unknown): void
  onErrors?(errors: string[]): void
}

/**
 * Shared JSON Schema rendering boundary.
 *
 * The form never enables `omitExtraData`. When only one top-level section is
 * displayed, only hidden top-level siblings are carried through the change
 * event. The visible section is authoritative, including deletions and array
 * order. RJSF resolves references and preserves its nested extra data itself.
 */
export function SchemaForm({
  schema,
  data,
  disabled = false,
  activeProperty = null,
  onChange,
  onErrors,
}: SchemaFormProps) {
  const visibleSchema = sectionSchema(prepareFormSchema(schema), activeProperty)
  const visibleData = sectionData(data, activeProperty)
  const uiSchema: UiSchema = {
    'ui:submitButtonOptions': { norender: true },
    'ui:options': { expandable: false },
    'ui:globalOptions': {
      label: true,
      orderable: true,
      copyable: false,
    },
  }

  return (
    <Form
      className="config-schema-form"
      schema={visibleSchema}
      validator={validator}
      formData={visibleData}
      uiSchema={uiSchema}
      templates={{
        DescriptionFieldTemplate: CollapsibleDescription,
        FieldTemplate: CompactField,
        ObjectFieldTemplate: CompactObject,
      }}
      formContext={{ activeProperty }}
      disabled={disabled}
      liveValidate="onBlur"
      showErrorList={false}
      focusOnFirstError
      omitExtraData={false}
      experimental_defaultFormStateBehavior={{
        emptyObjectFields: 'skipDefaults',
        arrayMinItems: { populate: 'never' },
        constAsDefaults: 'never',
      }}
      onChange={(event, id) => {
        // Initial/default notifications are not edits. In particular, merely
        // mounting a section must not rewrite a source draft or its comments.
        if (id === undefined) return
        if (activeProperty === null) {
          onChange(event.formData)
        } else {
          const siblings = { ...(data as Record<string, unknown>) }
          delete siblings[activeProperty]
          onChange({ ...siblings, ...(event.formData as Record<string, unknown>) })
        }
        onErrors?.(event.errors?.map(errorText) ?? [])
      }}
      onError={(errors) => onErrors?.(errors.map(errorText))}
    >
      <span />
    </Form>
  )
}

export function validateSchemaData(schema: RJSFSchema, data: unknown): string[] {
  try {
    return validator.validateFormData(data, schema).errors.map(errorText)
  } catch (error: unknown) {
    return [error instanceof Error ? error.message : String(error)]
  }
}

function sectionSchema(schema: RJSFSchema, property: string | null): RJSFSchema {
  if (property === null || schema.type !== 'object' || schema.properties?.[property] === undefined) {
    return schema
  }
  return {
    ...schema,
    properties: { [property]: schema.properties[property] },
    required: schema.required?.filter((name) => name === property),
    // Validate only this projection. The editor checks the complete draft
    // against the full schema, independently of which section is displayed.
    additionalProperties: true,
  }
}

function sectionData(data: unknown, property: string | null): unknown {
  if (
    property === null ||
    data === null ||
    typeof data !== 'object' ||
    Array.isArray(data) ||
    !(property in data)
  ) {
    return property === null ? data : {}
  }
  return { [property]: (data as Record<string, unknown>)[property] }
}

function errorText(error: RJSFValidationError): string {
  const path = error.property?.replace(/^\./, '') ?? ''
  return path.length > 0 ? `${path}：${error.message ?? '值无效'}` : (error.message ?? '值无效')
}

function CollapsibleDescription({ id, description }: DescriptionFieldProps) {
  if (description === undefined || description === null || description === '') return null
  return (
    <details className="field-description relative inline-block text-left">
      <summary aria-label="字段帮助" className="flex cursor-pointer list-none items-center rounded p-1 text-muted-foreground outline-none hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring">
        <Info aria-hidden="true" className="h-3.5 w-3.5" />
      </summary>
      <div id={id} className="relative z-10 my-1 max-w-lg whitespace-pre-wrap rounded-md border bg-muted/40 p-3 text-xs leading-5 text-muted-foreground">
        {String(description)}
      </div>
    </details>
  )
}

function CompactField(props: FieldTemplateProps) {
  const { id, label, children, errors, help, description, hidden, required, displayLabel, registry, uiSchema } = props
  const options = getUiOptions(uiSchema)
  const Wrap = getTemplate('WrapIfAdditionalTemplate', registry, options)
  if (hidden) return <div hidden>{children}</div>
  return (
    <Wrap {...props}>
      {displayLabel && options.widget !== 'checkbox' && (
        <div className="flex flex-wrap items-center gap-x-1.5">
          <label htmlFor={id} className="control-label">
            {label}{required && <span className="required" aria-hidden="true"> *</span>}
          </label>
          {description}
        </div>
      )}
      {children}
      {errors}
      {help}
    </Wrap>
  )
}

function CompactObject(props: ObjectFieldTemplateProps) {
  const { title, description, properties, fieldPathId, registry, schema, uiSchema, optionalDataControl, formData, disabled, readonly, onAddProperty } = props
  const { AddButton } = registry.templates.ButtonTemplates
  const activeProperty = registry.formContext?.activeProperty
  const sectionRoot = activeProperty !== null && activeProperty !== undefined &&
    (fieldPathId.path.length === 0 || (fieldPathId.path.length === 1 && fieldPathId.path[0] === activeProperty))
  return (
    <fieldset id={fieldPathId.$id} className="min-w-0">
      {!sectionRoot && title && <legend className="text-sm font-medium">{title}</legend>}
      {!sectionRoot && description && (
        <CollapsibleDescription
          id={`${fieldPathId.$id}__description`}
          description={description}
          schema={schema}
          uiSchema={uiSchema}
          registry={registry}
        />
      )}
      {optionalDataControl}
      <div className="grid min-w-0 grid-cols-1 gap-x-6 gap-y-4 lg:grid-cols-2">
        {properties.map((property) => {
          const declared = schema.properties?.[property.name]
          const field = typeof declared === 'object' ? registry.schemaUtils.retrieveSchema(declared) : undefined
          const type = field?.type
          const wide = type === 'object' || type === 'array' || type === undefined ||
            (Array.isArray(type) && (type.includes('array') || type.includes('object'))) ||
            /path|cmdline/.test(property.name)
          return (
            <div key={property.name} className={wide ? 'min-w-0 lg:col-span-2' : 'min-w-0'}>
              {property.content}
            </div>
          )
        })}
      </div>
      {canExpand(schema, uiSchema, formData) && (
        <AddButton
          id={buttonId(fieldPathId, 'add')}
          onClick={onAddProperty}
          disabled={disabled || readonly}
          uiSchema={uiSchema}
          registry={registry}
        />
      )}
    </fieldset>
  )
}
