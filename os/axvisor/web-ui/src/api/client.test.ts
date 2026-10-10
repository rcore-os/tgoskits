import { afterEach, describe, expect, it, vi } from 'vitest'
import { ApiClient, NO_RESPONSE_STATUS } from './client'
import { ApiError, describeError } from './types'

afterEach(() => {
  vi.unstubAllGlobals()
})

describe('ApiClient transport failures', () => {
  it('reports a connection that never reached the backend as status 0', async () => {
    // This is what the browser throws when the hypervisor is gone: a plain
    // `TypeError` with no status, which used to surface as `Failed to fetch`.
    vi.stubGlobal(
      'fetch',
      vi.fn(() => Promise.reject(new TypeError('Failed to fetch'))),
    )
    const client = new ApiClient()
    await expect(client.get('/api/vms')).rejects.toMatchObject({
      status: NO_RESPONSE_STATUS,
    })
  })

  it('describes the missing backend in words instead of the browser wording', () => {
    const described = describeError(new ApiError(NO_RESPONSE_STATUS, 'Failed to fetch'))
    expect(described).toContain('没有连上后端')
    expect(described).not.toContain('Failed to fetch')
  })

  it('keeps an aborted request as an abort, not as a backend failure', async () => {
    const abort = new DOMException('The operation was aborted.', 'AbortError')
    vi.stubGlobal(
      'fetch',
      vi.fn(() => Promise.reject(abort)),
    )
    const client = new ApiClient()
    await expect(client.get('/api/vms')).rejects.toBe(abort)
  })

  it('still reports an HTTP rejection with its status and detail', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(() =>
        Promise.resolve(
          new Response(JSON.stringify({ error: 'unknown VM' }), {
            status: 404,
            headers: { 'Content-Type': 'application/json' },
          }),
        ),
      ),
    )
    const client = new ApiClient()
    await expect(client.get('/api/vms/9')).rejects.toThrow('HTTP 404 · unknown VM')
  })
})
