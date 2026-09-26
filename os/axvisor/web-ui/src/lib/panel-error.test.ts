import { describe, expect, it } from 'vitest'
import { classifyPanelFailure, describePanelFailure } from './panel-error'

describe('classifyPanelFailure', () => {
  it('treats a stylesheet that could not be preloaded as a resource failure', () => {
    const error = new Error('Unable to preload CSS for /assets/Terminal-CFbL2ovg.css')
    expect(classifyPanelFailure(error)).toBe('resource')
    expect(describePanelFailure('resource')).toContain('没能从后端取回')
  })

  it('treats a chunk that could not be fetched as a resource failure', () => {
    expect(classifyPanelFailure(new Error('Failed to fetch dynamically imported module: /assets/VmsPanel-x.js'))).toBe(
      'resource',
    )
  })

  it('treats a refused connection as a resource failure even though fetch throws TypeError', () => {
    const error = new TypeError('Failed to fetch')
    expect(classifyPanelFailure(error)).toBe('resource')
  })

  it('treats a render-time type error as a contract failure', () => {
    // The `phys_cpu_set` regression: the field arrives as a number and the panel
    // called `.join` on it.
    const error = new TypeError('vcpu.phys_cpu_set.join is not a function')
    expect(classifyPanelFailure(error)).toBe('contract')
    expect(describePanelFailure('contract')).toContain('src/api/types.ts')
  })

  it('keeps an unrecognized failure out of both explanations', () => {
    const kind = classifyPanelFailure(new Error('something else went wrong'))
    expect(kind).toBe('unknown')
    expect(describePanelFailure(kind)).toContain('控制台')
  })

  it('classifies a non-Error value without throwing', () => {
    expect(classifyPanelFailure(undefined)).toBe('unknown')
    expect(classifyPanelFailure('Unable to preload CSS for /assets/x.css')).toBe('resource')
  })
})
