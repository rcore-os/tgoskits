import { describeError } from '@/api/types'
import { changeEditorMode, type EditorDraft } from './editor-draft'
import type { RJSFSchema } from '@rjsf/utils'

export type NormalizeConfig = (source: string, signal: AbortSignal) => Promise<string>

/** Owns only the pending conversion, not the editor's draft or API routing. */
export class VisualModeRequest {
  private generation = 0
  private controller: AbortController | null = null

  cancel(): void {
    this.generation += 1
    this.controller?.abort()
    this.controller = null
  }

  async enter(
    draft: EditorDraft,
    schema: RJSFSchema,
    normalize: NormalizeConfig | undefined,
    publish: (draft: EditorDraft) => void,
  ): Promise<void> {
    this.cancel()
    if (draft.mode === 'visual') return
    if (draft.source.trim().length === 0) {
      publish(changeEditorMode(draft, 'visual', schema))
      return
    }
    if (normalize === undefined) {
      publish({
        ...draft,
        normalizing: false,
        error: '当前后端不支持配置规范化，非空配置请使用 TOML 源码模式编辑',
      })
      return
    }

    const controller = new AbortController()
    const generation = this.generation
    this.controller = controller
    publish({ ...draft, normalizing: true, error: null })
    try {
      const canonical = await normalize(draft.source, controller.signal)
      if (controller.signal.aborted || generation !== this.generation) return
      publish(changeEditorMode({ ...draft, normalizing: false }, 'visual', schema, canonical))
    } catch (error: unknown) {
      if (controller.signal.aborted || generation !== this.generation) return
      publish({
        ...draft,
        normalizing: false,
        error: `无法进入可视化：${describeError(error)}；源码已保留，可继续编辑或重试`,
      })
    } finally {
      if (generation === this.generation) this.controller = null
    }
  }
}
