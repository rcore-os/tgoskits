# `ax-page-table-multiarch`（已退役）

`ax-page-table-multiarch` 已从 TGOSKits workspace 移除，不再作为现役 crate 维护。

架构无关的遍历、映射、解除映射、权限更新、查询和页表页生命周期已统一由 [`page-table-generic`](page-table-generic) 提供。具体 PTE 格式仍由 `ax-cpu`、`someboot` 或对应的虚拟化架构模块按页表阶段分别拥有。

迁移时不能只替换 crate 名称；请根据 [`page-table-generic` 的所有权与迁移说明](page-table-generic#迁移说明) 调整接口和具体 PTE 实现。
