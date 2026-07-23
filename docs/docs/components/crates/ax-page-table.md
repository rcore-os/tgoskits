# `ax-page-table`

> 路径：`memory/ax-page-table`

`ax-page-table` 是三个旧页表 crate 的能力合并结果，不是其中任意一个的简单改名：

| 旧组件 | 迁入位置 |
| --- | --- |
| `ax-page-table-entry` | `entry`：`MappingFlags`、`GenericPTE` 和架构 PTE |
| `ax-page-table-multiarch` | `stage1`：4 KiB 页表、cursor、protect、copy 和 TLB batching |
| `page-table-generic` | `stage2`/`boot` 共用的 flexible 页表机械能力 |

旧 package 名、feature alias 和 re-export 均不保留。统一核心按 feature 裁剪为互不调用的能力：

- `common`：唯一的 `PhysAddr`/`VirtAddr`、`PteConfig`、`PagingError`、`PageFrameProvider` 与 `TlbInvalidator` 契约。
- `entry`：跨架构 PTE、`MappingFlags` 与 AArch64 `MemAttrLayout`。
- `stage1`：内核/用户 4 KiB 页表、cursor、protect、copy 与 TLB batching。
- `stage2`：Guest 可变层级与可变页大小页表。
- `boot`：启动期 no-free 页帧来源与临时映射。

三个执行模块均使用 `common::PageFrameProvider` 注入页来源，crate 不依赖 `ax-alloc`。TLB 失效通过 `TlbInvalidator` 明确区分 local、硬件 broadcast 和 remote IPI。
