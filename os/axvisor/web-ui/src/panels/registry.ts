//! 渲染器注册表：kind → 懒加载组件。
//!
//! 不变量 6：新增一个面板 = panels/ 加目录 + 本文件加一行 + manifest 加一个节点，
//! 共三处，其余（壳、路由、client）零改动。
//! 面板之间零 import——vms（管理页）与 console（终端宿主）是两个独立组件，
//! 只共享壳注入的资源事件流；这就是「积木式组合，不焊接」。

import { lazy } from 'react'
import type { PanelComponent, PanelRegistry } from '@/api/types'
import { FallbackPanel } from './FallbackPanel'

// 懒加载：注册表里有 kind，不等于用户点了它——用到了才下载那一块代码
const VmsPanel = lazy(() => import('./vms/VmsPanel'))

const renderers: Record<string, PanelComponent> = {
  vms: VmsPanel, // 虚拟机管理页：创建/生命周期/列表
}

export function resolvePanel(kind: string): PanelComponent {
  return renderers[kind] ?? FallbackPanel
}

export const panelRegistry: PanelRegistry = { resolve: resolvePanel }
