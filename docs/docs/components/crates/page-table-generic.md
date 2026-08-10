# `page-table-generic`

> 路径：`memory/page-table-generic`
> 类型：库 crate
> 分层：组件层 / 架构无关页表执行引擎

`page-table-generic` 提供页表遍历、映射、解除映射、权限更新、查询、页表页生命周期和结构校验等通用能力。它只处理页表的树形结构，把页表项配置作为调用方提供的不透明关联类型，不选择当前架构，也不定义统一的硬件权限位或内存属性位。

## 所有权边界

- `page-table-generic`：通用遍历、映射流程、页表页生命周期、结构操作和错误。
- `ax-cpu`：运行时 Stage-1 的 `MappingFlags`、PTE 格式、页表几何、地址规范化和 TLB 失效。
- `someboot`：启动阶段的页表格式、配置和 MMU 交接。
- 虚拟化架构模块：Stage-2、EPT 和 NPT 的配置与 PTE 格式。
- `ax-hal`：运行时页表帧分配适配和页表类型别名，不解释 PTE 位。

这条边界保证通用内存层不会随着具体架构的 PTE 编码变化，也避免在 `memory/` 下重新引入多架构选择器。

## 主要接口

- `PageTable`：拥有根页表并提供映射、解除映射、查询和权限更新操作。
- `PageTableEntry`：由具体架构实现的结构操作接口，包含不透明的 `PteConfig`。
- `TableMeta`：描述页表层数、索引位数、基础页大小和最大块映射层级。
- `FrameAllocator`：页表页分配、释放和物理地址访问边界。
- `PagingError`：架构无关的页表操作错误；上层在 OS 或虚拟化边界转换错误类型。

## 迁移说明

仓库已经移除 `ax-page-table-entry` 和 `ax-page-table-multiarch`：

- 原通用页表执行能力迁移到 `page-table-generic`。
- 原运行时 Stage-1 页表项定义迁移到 `ax-cpu`。
- 启动页表项继续由 `someboot` 拥有。
- Stage-2、EPT 和 NPT 页表项由对应虚拟化架构模块拥有。

因此迁移不是简单的 crate 改名。调用方应先确认自己需要的是通用页表操作、运行时 Stage-1、启动页表还是虚拟化页表，再依赖相应的所有者。

## 验证

```bash
cargo test -p page-table-generic
cargo test -p page-table-generic --features copy-from
cargo xtask clippy --package page-table-generic
```
