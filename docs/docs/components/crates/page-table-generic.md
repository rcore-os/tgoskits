# `page-table-generic`

> 路径：`memory/page-table-generic`

`page-table-generic` 是无架构选择的页表遍历算法 crate。它不拥有主机、客户机或启动期的架构页表实现，也不通过 feature 聚合三个执行上下文。

| 能力 | 主要类型 | 所有者 |
| --- | --- | --- |
| 页表帧来源 | `PageFrameProvider` | 调用方实现 |
| 页表几何 | `TableMeta` | `axvm` 或 `someboot` 的架构模块 |
| 硬件条目转换 | `PageTableEntry`、`PteConfig` | `axvm` 或 `someboot` 的架构模块 |
| 递归操作 | `PageTable`、`PageTableRef`、`MapConfig` | 公共核心 |

主机第一阶段页表位于 `components/axcpu/src/paging/`，包括 `GenericPTE`、`MappingFlags`、`TlbInvalidator`、各架构页表项、32/64 位 cursor 和地址转换后备缓冲区批处理。客户机第二阶段页表的条目、几何和失效操作位于 `virtualization/axvm/src/arch/*/`。启动页表 adapter 和启用流程位于 `platforms/someboot/src/arch/*/`。

该 crate 直接依赖 `ax-memory-addr`，不依赖 `ax-alloc`、`axcpu`、`axvm` 或 `someboot`，源码不得按 `target_arch` 选择实现。生产直接消费者只有 `axcpu`、`axvm` 和 `someboot`；`axaddrspace` 仅在测试中使用。页表页来源由上层注入；分配失败立即返回 `PagingError::NoMemory`，核心不执行回收或重试。

完整设计、源码路径和示例见[页表分层与实现](../../architecture/memory/page-table)。
