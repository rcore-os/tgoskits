//! What this frontend must do with a manifest, without a browser.
//!
//! The accessor is the only place where a declaration becomes a request, so the
//! cases that matter are the ones that would otherwise turn into a wrong request
//! or a silent 404: a name this build does not declare, a template parameter
//! nobody passed, and the optional operation a build legitimately lacks.

import { describe, expect, it } from 'vitest'
import { Capabilities } from './accessor'
import type { PanelMeta } from '@/api/types'

const VMS: PanelMeta = {
  kind: 'vms',
  title: '虚拟机',
  root: '/api/vms',
  verbs: ['read', 'write'],
  links: [
    { name: 'list', verb: 'read', method: 'GET', href: '/api/vms' },
    { name: 'detail', verb: 'read', method: 'GET', href: '/api/vms/{id}' },
    { name: 'start', verb: 'write', method: 'POST', href: '/api/vms/{id}/start' },
    { name: 'delete', verb: 'write', method: 'DELETE', href: '/api/vms/{id}' },
    { name: 'browse', verb: 'read', method: 'GET', href: '/api/vms/browse' },
  ],
}

const SHELL: PanelMeta = {
  kind: 'shell',
  title: '管理终端',
  root: '/ws',
  verbs: ['read', 'write', 'stream'],
  links: [{ name: 'stream', verb: 'stream', method: 'GET', href: '/ws/{endpoint}' }],
}

/** One build declares the `fs`-only operations, one does not: the difference a panel sees. */
const WITH_POOL: PanelMeta = {
  ...VMS,
  links: [...VMS.links, { name: 'pool', verb: 'read', method: 'GET', href: '/api/vms/pool' }],
}

function capabilities(...panels: PanelMeta[]): Capabilities {
  return new Capabilities(panels)
}

describe('Capabilities.link', () => {
  it('returns the declared href of an operation that takes no parameter', () => {
    expect(capabilities(VMS).url('vms', 'list')).toBe('/api/vms')
  })

  it('fills template parameters and encodes them as one path segment', () => {
    const caps = capabilities(VMS, SHELL)
    expect(caps.url('vms', 'start', { id: 42 })).toBe('/api/vms/42/start')
    expect(caps.url('shell', 'stream', { endpoint: 'vm 1/../x' })).toBe('/ws/vm%201%2F..%2Fx')
  })

  it('throws on an operation this build does not declare, naming what it has', () => {
    // Copy-paste-and-forget is the failure this prevents: `pause` exists on the
    // real table but not in this fixture, and a 404 would look like a backend fault.
    const caps = capabilities(VMS)
    expect(() => caps.url('vms', 'pause', { id: 1 })).toThrowError(/没有「pause」/)
    expect(() => caps.url('vms', 'pause', { id: 1 })).toThrowError(/list/)
  })

  it('throws on a panel this build does not declare', () => {
    expect(() => capabilities(VMS).url('files', 'list')).toThrowError(/没有「files」/)
  })

  it('throws when a template parameter is missing instead of sending a literal brace', () => {
    expect(() => capabilities(VMS).url('vms', 'detail')).toThrowError(/需要参数「id」/)
  })
})

describe('Capabilities.maybeUrl', () => {
  it('is null for an operation this build lacks and a URL for one it has', () => {
    expect(capabilities(VMS).maybeUrl('vms', 'pool')).toBeNull()
    expect(capabilities(WITH_POOL).maybeUrl('vms', 'pool')).toBe('/api/vms/pool')
  })

  it('reports the same presence through declared()', () => {
    expect(capabilities(VMS).declared('vms', 'pool')).toBe(false)
    expect(capabilities(WITH_POOL).declared('vms', 'pool')).toBe(true)
    expect(capabilities(VMS).declared('vms', 'browse')).toBe(true)
  })
})

describe('Capabilities.bind', () => {
  it('binds a panel to its own resource and keeps the accessor stable across renders', () => {
    const caps = capabilities(VMS, SHELL)
    const bound = caps.bind('vms')
    expect(bound).toBe(caps.bind('vms'))
    expect(bound.url('list')).toBe('/api/vms')
    expect(bound.declared('pool')).toBe(false)
    expect(bound.maybeUrl('pool')).toBeNull()
  })

  it('refuses to bind a kind that was not declared', () => {
    // The shell binds what the manifest listed, so this only fires on a bug in
    // the shell itself — and then it must not return an accessor that throws later.
    expect(() => capabilities(VMS).bind('files')).toThrowError(/未声明的面板「files」/)
  })
})

describe('Capabilities navigation helpers', () => {
  it('lists kinds in declaration order and reports declared verbs', () => {
    const caps = capabilities(VMS, SHELL)
    expect(caps.kinds()).toEqual(['vms', 'shell'])
    expect(caps.verbs('shell')).toEqual(['read', 'write', 'stream'])
    expect(caps.verbs('files')).toEqual([])
    expect(caps.panel('files')).toBeNull()
  })
})
