# 统一页表执行器与架构边界

## 问题

仓库同时存在 `ax-page-table-entry`、`ax-page-table-multiarch` 和
`page-table-generic` 两套页表执行路径。ArceOS/StarryOS 使用前两者，
`someboot`、`somehal`、`axaddrspace` 和 `axvm` 使用后者。这使相同的映射、
遍历、错误处理和页表页生命周期需要维护两份实现，并让通用 `memory/`
目录承担了所有目标架构的 PTE 选择。

本次变更的调用方是 ArceOS/StarryOS 运行时内存管理。成功标准是：

- 所有生产页表调用方统一使用 `page-table-generic`；
- 运行时 stage-1 的 PTE 格式、页表层级和 TLB 刷新语义由 `ax-cpu` 提供；
- `ax-hal` 不再选择架构，只提供运行时页表页分配和物理地址访问适配；
- 四个现有架构的 4 KiB/2 MiB/1 GiB 映射、权限、内存属性和缺页访问类型保持兼容；
- `ax-page-table-entry` 和 `ax-page-table-multiarch` 本次保留，待迁移验证完成后单独删除。

本次非目标包括改变 Linux `mmap`/`mprotect`/COW 可见语义、改变 stage-2
虚拟化格式、删除旧 crate，以及引入新的页表页分配策略。

## 边界

页表能力按其变化原因归属：

| 边界 | 所有能力 | 不应包含 |
| --- | --- | --- |
| `memory/page-table-generic` | 地址与 flags 的通用表达、页表页生命周期、映射/取消映射/查询/保护/复制、通用错误 | 目标架构选择、架构寄存器、具体 PTE 位布局 |
| `components/axcpu` | 运行时 stage-1 PTE 位布局、页表层级元数据、虚拟地址 canonicalization、TLB 刷新、缺页访问类型 | 页表页分配、地址空间区间策略 |
| `platforms/someboot` | 启动阶段使用的架构页表描述符和 MMU 切换顺序 | 运行时地址空间策略 |
| `virtualization/axvm` | stage-2/EPT/NPT 格式及其失效语义 | 宿主 stage-1 选择 |
| `os/arceos/modules/axhal` | 用全局页分配器实现 `FrameAllocator`，导出当前架构页表别名 | 架构枚举和 PTE 位定义 |
| `ax-mm` / StarryOS mm | 映射区间、COW、RSS、错误转换和用户态策略 | 架构寄存器与 PTE 位布局 |

依赖方向为：

```text
ax-mm / StarryOS mm
        |
      ax-hal --------> ax-cpu::paging
        |                    |
        +------> page-table-generic

someboot ------------------> page-table-generic
axvm stage-2 --------------> page-table-generic
```

`page-table-generic` 不依赖 `ax-cpu`。泛型执行器通过 `TableMeta` 和
`PageTableEntry` 接收调用方拥有的具体格式，因此启动、宿主 stage-1 与 guest
stage-2 可以共享执行器而不互相依赖。

## 方案与替代方案

选择扩展现有 `page-table-generic`，补齐旧执行器调用方实际使用的
`map_page`、`unmap_page`、`protect_region`、root-entry 共享/复制和批量 TLB
失效能力，再迁移调用方。

- 保持两套执行器会继续复制修复和架构适配，不满足统一目标。
- 把所有架构实现继续放在 `memory/` 中，仍会使架构变更同时修改通用内存组件。
- 新建第四个统一 crate 会扩大公共面，并重复现有 generic trait 边界。
- 将启动和 stage-2 格式也全部放入 `ax-cpu` 会混合不同页表阶段及生命周期；它们继续由真实消费者拥有。

该方案的代价是 `page-table-generic` 需要暂时提供与旧调用面等价的高层操作，
并在迁移期同时保留旧 crate。这个代价可通过依赖检查明确观察，并可在后续独立
删除提交中回收。

## 迁移与兼容

迁移保持 `MappingFlags` 的位语义，并把 CPU trap 报告的
`PageFaultFlags` 与映射属性分离。地址空间在进入映射策略前显式完成转换，避免把
`DEVICE`、`UNCACHED` 等 PTE 属性误当成 fault 来源。

执行器区分“页表槽未使用”与“硬件 PTE valid/present”。空权限映射可以保留物理地址
但保持 non-present，以支持 `PROT_NONE` 和按需映射；`protect`/`remap` 可激活这类叶项，
`unmap` 则必须清除其占用状态。具体架构通过 `PageTableEntry::unused` 报告该区别，
通用执行器不推断 PTE 位布局。

内核地址空间创建时先建立受管映射，再从 boot 页表深拷贝缺失的根项；用户地址
空间只在平台需要时借用内核根项。借用范围在页表销毁前解除，防止释放内核拥有的
页表页。

旧 crate 的源码、workspace 依赖项和发布历史本次不删除。完成四架构构建、
targeted clippy、generic 单元测试和至少一条 ArceOS/StarryOS QEMU 路径后，再用
独立变更删除它们。

## 验证

- `page-table-generic` 单元测试覆盖权限转换、各级映射、复制/共享、失败清理、
  root entry、canonical address 与 targeted/full TLB invalidation。
- `ax-cpu` 的架构测试锁定 LoongArch leaf/table/huge/global 位语义；其余架构通过
  对应 target 的 clippy/构建验证实际条件编译实现。
- Cargo metadata 依赖检查必须证明除保留的 legacy crate 自身外，没有生产包依赖
  `ax-page-table-entry` 或 `ax-page-table-multiarch`。
- ArceOS 和 StarryOS 的 QEMU 验证必须到达各自既有成功条件；本次不放宽测试 regex
  或 Linux ABI 断言。
