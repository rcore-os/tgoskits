---
slug: introducing-tgoskits
title: 认识 TGOSKits：统一组织 ArceOS、StarryOS 与 Axvisor 的工作区
authors: [tgoskits-team]
tags: [tgoskits, workspace, arceos, starryos, axvisor]
---

TGOSKits 是一个面向操作系统与虚拟化开发的统一工作区。它把 ArceOS、StarryOS、Axvisor 以及大量共享组件 crate 组织到同一个仓库中，让系统开发、平台适配、测试验证与文档维护可以围绕同一套目录结构与构建入口协同演进。

<!-- truncate -->

## 为什么会有 TGOSKits

在多系统并行演进的场景里，最常见的问题不是“缺少代码”，而是：

- 共享能力分散在不同位置，修改影响面难以判断
- 构建入口和验证方式不统一，新成员上手成本高
- 文档和代码脱节，难以快速定位该看哪一层

TGOSKits 的目标，就是把这些系统级研发动作放进一个统一语境里处理。

## TGOSKits 包含什么

当前工作区主要围绕三条系统路径和一层共享组件展开：

- `ArceOS`：模块化内核路径，提供大量基础系统能力
- `StarryOS`：建立在 ArceOS 之上的 Linux 兼容系统路径
- `Axvisor`：基于 ArceOS 的 Type-I Hypervisor 路径
- `components/*`：被多个系统复用的基础 crate

这些内容通过统一的仓库结构和文档站点串联起来，使“从组件到系统”的分析路径更清晰。

## 统一入口带来的收益

TGOSKits 在工程层面强调几件事：

- 用统一文档入口组织概览、设计、参考资料和系统指南
- 用 `cargo xtask` 作为推荐构建入口，减少分散脚本带来的认知切换
- 用共享组件视角帮助判断改动影响面
- 用从 host 测试到系统级运行的验证链路收束工程质量

这意味着无论你是在改一个基础 crate、修一个平台适配问题，还是调试 Axvisor 的 Guest 路径，都能更容易找到对应的入口和验证方法。

## 推荐的阅读方式

如果这是你第一次进入仓库，建议按下面顺序浏览：

1. [项目概览](/docs/introduction/overview)
2. [快速开始](/docs/design/reference/quick-start)
3. [仓库结构](/docs/design/reference/repo)
4. [组件开发指南](/docs/design/reference/components)
5. 对应系统指南：[ArceOS](/docs/design/systems/arceos-guide)、[StarryOS](/docs/design/systems/starryos-guide)、[Axvisor](/docs/design/systems/axvisor-guide)

## 后续会在 Blog 中写什么

当前 blog 先保留这一篇示例文章，作为当前项目介绍入口。后续如果继续扩展，适合放在这里的内容包括：

- 工作区结构和构建链的演进记录
- 某一类共享组件的设计复盘
- 系统级验证或平台适配经验总结
- 文档体系与开发流程的重要更新

如果你正在浏览 TGOSKits 文档站，欢迎从 [Community](/community/introduction) 了解参与方式，也欢迎直接到 GitHub 仓库跟进项目进展。
