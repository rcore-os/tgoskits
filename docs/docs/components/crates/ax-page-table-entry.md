# `ax-page-table-entry`（已退役）

`ax-page-table-entry` 已从 TGOSKits workspace 移除，不再作为现役 crate 维护。

原有职责已按页表所有权拆分：运行时 Stage-1 的 PTE 格式和映射权限属于 `ax-cpu`，启动页表格式属于 `someboot`，Stage-2、EPT 和 NPT 格式属于对应的虚拟化架构模块。通用页表结构操作由 [`page-table-generic`](page-table-generic) 提供。

这不是一对一的 crate 改名。迁移调用方时，应先确认所使用页表的阶段和所有者，再选择对应组件。
