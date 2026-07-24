# `axaddrspace`

`axaddrspace` 是位于 `virtualization/axaddrspace` 的 `no_std` 库 crate，当前版本为 `0.5.17`。它管理 Guest Physical Address（客户机物理地址，GPA）区域、客户机 RAM 后端和第二阶段映射，但不选择具体架构页表，也不直接依赖生产环境的宿主页分配器。

## 1. 组件索引

本页只记录 crate 的稳定边界和源码入口。后端算法、直接映射语义、架构适配、锁和生命周期由架构文档统一维护，避免组件目录复制另一套实现说明。

### 1.1 定位与依赖

生产依赖由地址类型、虚拟区域容器和基础支持库组成。`page-table-generic` 只作为开发依赖构造 mock nested table，生产环境由 `axvm` 组合具体第二阶段页表和 `axaddrspace`。

| 项目 | 内容 |
| --- | --- |
| Crate 路径 | `virtualization/axaddrspace` |
| 分层 | 虚拟化策略层 / 客户机物理地址空间 |
| 公共机制依赖 | `ax-memory-addr`、`ax-memory-set`、`axvm-types` |
| 主要消费者 | `axvm`、`axdevice`、`axvisor_api`、Axvisor |
| 不直接依赖 | 生产页表实现、`ax-alloc`、ArceOS `ax-mm` |

`axaddrspace` 通过 `NestedPageTableOps` 接收页表、宿主页和物理地址转换能力。具体架构 adapter 位于 `virtualization/axvm/src/arch/` 与 `virtualization/axvm/src/npt.rs`。

### 1.2 公共入口

`src/lib.rs` 只重导出地址空间、后端、错误、客户机内存访问能力和嵌套页表接口。

| 入口 | 用途 |
| --- | --- |
| `AddrSpace<Npt>` | 保存 GPA 总范围、区域集合和嵌套页表实例 |
| `Backend<Npt>` | 表达 Linear 与 Alloc 两类客户机 RAM 策略 |
| `NestedPageTableOps` | 注入页表根、frame、map/unmap/protect/query 能力 |
| `PageSize` | 表达查询和恢复使用的实际页尺寸 |
| `GuestMemoryAccessor` | 为客户机对象和 buffer 访问提供 capability |
| `AddrSpaceError` / `AddrSpaceResult` | 返回可匹配的范围、映射和访问错误 |

地址、范围和 `MappingFlags` 来自 `axvm-types`，本 crate 不重复定义 GPA 或虚拟机公共权限类型。

## 2. 文档入口

组件使用者应按问题所在层级阅读对应文档。架构文档以当前源码为准说明完整链路，本页不保存测试命令或算法副本。

### 2.1 组件实现

客户机地址空间的字段、Linear/Alloc 所有权、单次 map 失败清理、懒分配缺页、四种架构 adapter、AxVM 外层锁、不安全 slice 前置条件和销毁顺序见[Axvisor 客户机地址空间设计与实现](../../architecture/memory/axaddrspace.md)。

通用 `MemorySet` 的 `BTreeMap` 区域管理与直接 backend 协议见[虚拟内存区域管理](../../architecture/memory/address-space.md)，页表遍历和页表项机制见[页表分层与实现](../../architecture/memory/page-table.md)。

### 2.2 验证与集成

frame 释放、巨大 Linear 映射和各架构构建要求见[内存管理测试与验收](../../architecture/memory/testing.md)。ArceOS、StarryOS 与 Axvisor 的整体接线见[系统内存集成](../../architecture/memory/integration.md)。

修改 `NestedPageTableOps` 或 `MappingBackend` 时，必须同时检查 `virtualization/axvm/src/npt.rs`、四个架构适配器和 `ax-memory-set` 的全部实现，不能通过旧 trait alias 或转发模块保留双协议。
