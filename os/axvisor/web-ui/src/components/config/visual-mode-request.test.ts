import { afterEach, describe, expect, it, vi } from 'vitest'
import type { RJSFSchema } from '@rjsf/utils'
import { ApiClient } from '@/api/client'
import guestSchema from '@/generated/guest-config.schema.json'
import { createConfigNormalizer } from './ConfigDialog'
import { changeEditorMode, editEditorSource, editEditorVisual } from './editor-draft'
import { VisualModeRequest } from './visual-mode-request'

const schema = guestSchema as RJSFSchema

function deferred<T>() {
  let resolve!: (value: T) => void
  let reject!: (reason: unknown) => void
  const promise = new Promise<T>((accept, refuse) => { resolve = accept; reject = refuse })
  return { promise, resolve, reject }
}

afterEach(() => vi.unstubAllGlobals())

describe('visual normalization request boundary', () => {
  it('posts original source to the injected capability, uses canonical form data, and normalizes again after an edit', async () => {
    const source = '# compatible tuple\n[base]\nid = 7\n[kernel]\nmemory_regions = [[4096, 8192, 7, 0]]\n'
    const canonical = '[base]\nid = 7\n[kernel]\n[[kernel.memory_regions]]\ngpa = 4096\nsize = 8192\nflags = 7\nmap_type = 0\n'
    const response = deferred<Response>()
    const fetcher = vi.fn().mockReturnValueOnce(response.promise)
    vi.stubGlobal('fetch', fetcher)
    const normalize = createConfigNormalizer(new ApiClient(), '/declared/normalize-configuration')
    const request = new VisualModeRequest()
    let draft = editEditorSource(source)
    const task = request.enter(draft, schema, normalize, (next) => { draft = next })
    expect(draft).toMatchObject({ mode: 'source', source, normalizing: true, valid: true })
    const [url, init] = fetcher.mock.calls[0] as [string, RequestInit]
    expect(url).toBe('/declared/normalize-configuration')
    expect(init).toMatchObject({ method: 'POST', body: JSON.stringify({ toml: source }) })
    expect(init.signal).toBeInstanceOf(AbortSignal)
    response.resolve(new Response(JSON.stringify({ toml: canonical })))
    await task
    expect(draft).toMatchObject({
      mode: 'visual', source, normalizing: false, valid: true, schemaErrors: [],
      formData: { kernel: { memory_regions: [{ gpa: 4096, size: 8192, flags: 7, map_type: 0 }] } },
    })
    draft = changeEditorMode(draft, 'source', schema)
    expect(draft.source).toBe(source)
    // Returning from source must use the endpoint again, including for a
    // source produced by the visual editor itself.
    draft = changeEditorMode(draft, 'visual', schema, canonical)
    const changed = structuredClone(draft.formData) as { base: { id: number } }
    changed.base.id = 8
    draft = editEditorVisual(draft, changed, schema)
    const editedSource = draft.source
    draft = changeEditorMode(draft, 'source', schema)
    fetcher.mockResolvedValueOnce(new Response(JSON.stringify({ toml: editedSource })))
    await request.enter(draft, schema, normalize, (next) => { draft = next })
    expect(JSON.parse(fetcher.mock.calls[1][1].body)).toEqual({ toml: editedSource })
    expect(draft).toMatchObject({ mode: 'visual', source: editedSource, valid: true, formData: { base: { id: 8 } } })
  })

  it('preserves editable source on transport failure and leaves old backends source-only without blocking blank new drafts', async () => {
    vi.stubGlobal('fetch', vi.fn().mockRejectedValue(new TypeError('connection lost')))
    const request = new VisualModeRequest()
    const source = '[base]\nid = 7\n'
    let draft = editEditorSource(source)
    await request.enter(draft, schema, createConfigNormalizer(new ApiClient(), '/declared/normalize'), (next) => { draft = next })
    expect(draft).toMatchObject({ source, mode: 'source', normalizing: false, valid: true })
    expect(draft.error).toContain('源码已保留')
    const absent = createConfigNormalizer(new ApiClient(), null)
    await request.enter(draft, schema, absent, (next) => { draft = next })
    expect(draft).toMatchObject({ source, mode: 'source', valid: true })
    expect(draft.error).toContain('不支持配置规范化')
    draft = editEditorSource(' \n')
    const normalize = vi.fn()
    await request.enter(draft, schema, normalize, (next) => { draft = next })
    expect(normalize).not.toHaveBeenCalled()
    expect(draft).toMatchObject({ source: ' \n', mode: 'visual', normalizing: false, valid: true })
  })

  it.each(['success', 'failure'])('ignores late %s after cancellation and source editing, even if transport ignores abort', async (outcome) => {
    const response = deferred<string>()
    const request = new VisualModeRequest()
    let signal!: AbortSignal
    let draft = editEditorSource('[base]\nid = 7\n')
    const task = request.enter(draft, schema, async (_source, currentSignal) => {
      signal = currentSignal
      return response.promise
    }, (next) => { draft = next })
    request.cancel()
    draft = editEditorSource('[base]\nid = 8\n')
    expect(signal.aborted).toBe(true)
    if (outcome === 'success') response.resolve('[base]\nid = 7\n')
    else response.reject(new Error('late refusal'))
    await task
    expect(draft).toEqual(editEditorSource('[base]\nid = 8\n'))
  })

  it('keeps the newest normalization when older requests finish out of order', async () => {
    const first = deferred<string>()
    const second = deferred<string>()
    const normalize = vi.fn().mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise)
    const request = new VisualModeRequest()
    let draft = editEditorSource('[base]\nid = 7\n')
    const old = request.enter(draft, schema, normalize, (next) => { draft = next })
    draft = editEditorSource('[base]\nid = 8\n')
    const current = request.enter(draft, schema, normalize, (next) => { draft = next })
    expect((normalize.mock.calls[0][1] as AbortSignal).aborted).toBe(true)
    second.resolve('[base]\nid = 8\n')
    await current
    first.resolve('[base]\nid = 7\n')
    await old
    expect(draft).toMatchObject({ mode: 'visual', source: '[base]\nid = 8\n', normalizing: false, formData: { base: { id: 8 } } })
  })
})
