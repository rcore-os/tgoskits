---
sidebar_position: 10
sidebar_label: "测试、性能与限制"
---

# 内存管理测试、性能与当前限制

内存修改必须同时验证区间事实、资源 ownership、失败回滚、跨 CPU 可见性和热路径延迟。测试优先使用可控 allocator/page-table/backend 和 tracking DMA adapter，板级测试负责固件内存图、cache、TLB 与设备行为。

## 1. 启动与 allocator 测试

启动内存和 allocator 的错误会污染所有上层测试，因此需要先验证物理总量和不重叠，再测试分配行为。

### 1.1 启动内存图

确定性输入应包含至少两段 RAM、一个跨 Free 中部的 reservation、KImage、early bump used prefix 和 MMIO hole。输出逐段比较，而不是只比较总大小。

| 用例 | 断言 |
| --- | --- |
| Free 中插入 Reserved | Free 被 split，Reserved 精确保留 |
| 相邻同类型 reservation | 合并为单一描述符 |
| 不同 non-Free overlap | 返回 `Conflict`，原 map 不变 |
| fixed capacity exhausted | 返回 `Capacity`，原 map 不变 |
| range/alignment overflow | 返回 typed error，无 wrapping range |
| 多 memory nodes/regions | 每个合法 bank 都进入最终 Free 列表 |
| early bump freeze | used prefix 变 Reserved，freeze 后拒绝 allocation |

板级启动日志应保存固件输入、someboot memory map、ax-hal normalized regions和 Buddy managed sections，逐层核对丢失的 bytes 属于哪类 metadata或对齐。

### 1.2 Buddy 与 Slab

host test 使用可控内存 slice 建立多个 section，覆盖 page、lowmem、small object、large object 和 remote-free。

| 用例 | 断言 |
| --- | --- |
| add disjoint regions | section count 和 managed bytes正确 |
| add overlap | 返回 `MemoryOverlap` |
| too-small added region | 明确 skip，不产生无效 section |
| Normal page | size/alignment 满足，free 后可合并 |
| Dma32 page | allocation 最后一个 byte 小于 4 GiB |
| large contiguous | 不跨 section；无单 section 可满足时 OOM |
| all size classes | round-up、bitmap 和 empty-slab return正确 |
| cross-CPU free | remote stack只消费一次，owner drain 后可复用 |

分配失败不得触发 callback、VFS、阻塞或隐式 retry。使用 fake reclaim counter 可以证明 `ax-alloc` 完全不调用上层回收。

## 2. 页表与地址空间测试

页表测试验证单个机制，地址空间测试验证多个机制组合后的 all-or-rollback。两者不能互相替代。

### 2.1 页表能力

每个架构 entry 应做 flags/PTE round-trip，每个 engine 应做 map/query/protect/unmap 和 ownership teardown。

| 用例 | 断言 |
| --- | --- |
| entry round-trip | PA、permission、device/uncached 与 huge bit 不丢失 |
| AArch64 MAIR layout | boot/EL1/EL2/PTE 共用同一 index/value |
| frame allocation failure | 返回 `PagingError::NoMemory`，不留下 half-linked table |
| map conflict | 旧 PTE 保持不变 |
| huge mapping | alignment、level 和 translate offset正确 |
| cursor batching | 1..32 地址逐项 flush，超过阈值 full flush |
| SMP validation | local-only + multi CPU + no IPI 初始化失败 |
| owned table Drop | 每个 owned child frame只释放一次 |

Stage-1、Stage-2 和 boot 需要分别构建，证明 feature 之间没有隐式调用。boot provider 的 no-free 语义应单独测试，不能用 runtime provider 的 Drop 预期套用。

### 2.2 Mapping transaction

`ax-memory-set` 的 fault backend 应允许指定第 N 个 prepare、commit 或 rollback 失败，并保存操作前完整 metadata/PTE snapshot。

| 故障点 | 成功标准 |
| --- | --- |
| metadata Vec reserve | live VMA/PTE 不变 |
| 中间 prepare | 已准备 plan 全部 reverse abort |
| 中间 map commit | 本 operation 自恢复，前序 operation rollback |
| 多 VMA unmap | 所有旧 PTE 和 area ranges 恢复 |
| protect split | actual/reported flags 与 PTE 一致恢复 |
| deferred frame release | 仅完整成功后 finalize释放 |
| rollback failure | 返回 `BadState`，不伪报普通可恢复错误 |

ArceOS、Starry 和 axaddrspace backend 都需要运行同一语义级用例，并增加各自的 frame、RSS/COW 或 Guest RAM ownership 断言。

## 3. Starry 与 DMA 测试

Starry 和 DMA 都涉及跨对象生命周期，必须测试 close/unmap/fork/timeout 等非正常顺序。

### 3.1 Starry VM

Starry 测试要同时比较用户可见结果和内部计数。涉及 syscall 语义时，测试期望以 Linux/POSIX 行为为基准。

| 用例 | 断言 |
| --- | --- |
| RLIMIT_AS replacement | 只计算 retained + requested，超限不改 VMA |
| Always overcommit | mode=1，超 commit limit 仍可 reserve并准确报告 |
| Strict overcommit | mode=2，超 limit 返回 ENOMEM 路径 |
| commit 分类 | private anonymous/private file 仅 writable 计入；shared-anonymous owner 计入一次；file/device/imported memory 不计入 |
| fork COW overflow | checked `u32` 拒绝且 count不变 |
| fork 中间失败 | parent flags/refs/RSS 与 child resources回滚 |
| private file read→write | RSS File 迁移到 Anon |
| mremap move | PTE 与 charge key 同步移动 |
| fault clean reclaim | 只对 OOM回收一次、最多 retry一次 |
| proc status/statm | VSS、RSS categories、peak和 stack分类一致 |

Starry 直接发现的 QEMU/board case 应覆盖多线程 fault、fork/exec/exit、memfd/shared mapping、MAP_FIXED 和跨 VMA `mprotect`。重型内存压力负载放在 `apps/starry`，通过 `cargo xtask starry app` 执行，不恢复已删除的 `normal`/`stress` 一级分组。

### 3.2 DMA ownership

DMA host test 使用 tracking `DmaOp` 记录 allocation、free、map、unmap和 cache sync。硬件测试验证实际 cache和设备 completion。

| 用例 | 断言 |
| --- | --- |
| invalid `DmaPod` compile-fail | reference/resource类型无法进入 typed buffer |
| token Copy/Clone compile-fail | free/unmap token不可复制 |
| RAII Drop | coherent、contiguous、streaming 各释放一次 |
| backend violates mask | token先被释放，再返回 typed error |
| bounce direction | copy-in/out 和 clean/invalidate 顺序正确 |
| fd close before mmap unmap | dma-buf backing由 mmap retainer保留 |
| accelerator import | operation结束前 owner live，driver不释放 |
| reset/timeout | quiesce 后 complete；无法证明时 quarantine/leak |

`qemu-rga/system/rga-lifecycle` 和 dma-buf 最后引用释放用例应作为设备路径回归；物理板还要验证 JPEG/NPU/TPU 使用同一 Dma32 ownership模型。

## 4. 验证命令

内存 crate 的 host test可以使用 Cargo；ArceOS、StarryOS和 Axvisor 的系统构建/运行应使用 `cargo xtask`。文档改动另外执行 Docusaurus build。

### 4.1 Targeted crate 验证

修改单一 crate 时先运行格式、该 crate clippy和对应 unit/doc tests。以下命令是常用最小集合，feature 应按改动补齐。

```sh
cargo fmt --all --check
cargo xtask clippy --package ax-alloc
cargo xtask clippy --package ax-page-table
cargo xtask clippy --package ax-memory-set
cargo xtask clippy --package starry-mm
cargo xtask clippy --package dma-api
cargo test -p ax-memory-set
cargo test -p starry-mm
cargo test -p dma-api
```

修改 `ax-page-table` 时要分别验证 `stage1`、`stage2`、`boot` feature 组合；修改 `ax-alloc` 时覆盖实际存在的 `global-allocator`、`tracking` 和 `smp` 组合。hard-RT 与 reserve 尚不是 Cargo feature，只有增加真实消费者和构建配置后才加入对应矩阵。

### 4.2 Workspace 与系统验证

依赖或 feature 改动需要检查 workspace metadata 和生产 dependency tree。系统命令以仓库 `cargo xtask --help` 和现有 CI配置为准。

```sh
cargo metadata --format-version 1
cargo tree --workspace
# 按整改方案中的删除清单检查生产源码、manifest 和依赖树。
npm --prefix docs run build
git diff --check
```

ArceOS、StarryOS 和 Axvisor 至少各选择一个 paging 配置构建；Starry 另运行直接发现的内存相关 QEMU case，重型压力负载通过 `cargo xtask starry app` 执行。物理 board、自托管 runner 和设备压力测试按变更范围执行。

## 5. 性能与容量指标

性能基线必须使用相同平台、CPU数、内存图、feature和 workload。平均值不能替代 P99/max，因为 RT和 IRQ路径关注最坏延迟。

### 5.1 allocator 指标

allocator benchmark 分开记录 Slab、Buddy order-0、高阶连续页、Dma32和 cross-CPU free。统计至少包括延迟、锁等待和空间开销。

| 指标 | 采集维度 | 验收目标 |
| --- | --- | --- |
| alloc/free latency | median、P99、max | 相同配置 P99相对基线退化不超过 10% |
| Buddy lock wait | CPU、operation size | 证明是否需要后续 cache优化 |
| remote-free drain | queue length、drain latency | 无双重释放或长期不回收 |
| fragmentation | largest allocatable block、free pages | 压力后仍满足目标高阶请求 |
| metadata | 每 section prefix、每页 metadata | 小内存板可接受 |
| managed/physical ratio | 输入 Free 与 `managed_bytes()` | 解释对齐与 metadata损失 |
| image/static state | feature组合 | 关闭能力后不进入镜像 |

只有固定池和批量预分配仍不能满足板级绝对延迟、且采样证明 Buddy锁是主要瓶颈时，才考虑可 drain的有限 per-CPU order-0 cache。

### 5.2 系统指标

地址空间、页表、Starry和 DMA有各自额外指标。测量时应记录失败路径和回收次数，而不只记录成功吞吐。

| 领域 | 指标 |
| --- | --- |
| 页表 | map/unmap/protect latency、分配的 table frames、TLB address/full flush次数 |
| Starry | minor fault P99/max、COW copy、reclaim pages/attempts、RSS drift、fork/mremap latency |
| Guest | NPT fault、Guest RAM populate/teardown、huge mapping比例 |
| DMA | alloc/map/unmap latency、cache sync bytes、bounce次数/bytes、quarantine/leak |
| hard-RT | critical section通用 heap/page allocation次数 |
| boot | memory map处理时间、early bump bytes、per-CPU固定开销 |

hard-RT 的关键验收是已识别 RT critical section 的通用堆和页分配次数为 0。驱动 ring/descriptor 应在 probe 或启动期预分配。

## 6. 静态架构检查

代码通过测试并不证明没有旧入口或反向依赖。每次内存重构都要用 source/dependency scan 检查组件边界。

### 6.1 旧代码清理

生产依赖中不应再出现已删除 allocator、DMA或页表 crate。历史 CHANGELOG可以保留名称，但不能被 workspace member、feature或 re-export重新引入。

| 检查项 | 失败示例 |
| --- | --- |
| 已删除 package | Cargo.toml 或 Cargo.lock 仍包含已删除的 allocator、DMA 或页表 package |
| compatibility re-export | 新 crate重新导出旧 module/type alias |
| duplicate allocator入口 | driver直接依赖 `buddy-slab-allocator` |
| reverse dependency | `ax-page-table` 依赖 `ax-alloc` |
| duplicate stats | proc/kernel维护另一套 allocator usage counters |
| legacy DMA release | 地址/页数/bool 分离，或 token实现 Copy/Clone |

Cargo.lock冲突不手工合并；依赖冲突解决后由 Cargo重新生成并检查 diff。

### 6.2 Feature 裁剪

分别生成最小 ArceOS、SMP ArceOS、Starry和 Axvisor dependency tree，确认执行模块只在需要时链接。

| 构建 | 应存在 | 不应存在 |
| --- | --- | --- |
| embedded ArceOS | `ax-alloc`、必要 Stage-1 | Starry policy、Stage-2、unused reserve |
| Starry | Stage-1、`starry-mm`、Linux backend | Stage-2 |
| Axvisor | Stage-2、`axaddrspace` | Starry policy、boot engine runtime copy |
| boot-only platform component | `ax-page-table/boot` | runtime allocator依赖循环 |

feature scan还要比较静态符号和镜像大小，避免关闭 feature 后只隐藏 API但仍保留全局状态。

## 7. 当前设计约束

以下内容是嵌入式性能和复杂度取舍，不应被当成缺失功能自动补齐。

| 约束 | 理由 |
| --- | --- |
| 单全局 Buddy锁 | 简单、metadata少；测量触发前不加完整 PCP |
| 连续 allocation不跨 section | 保证真实物理连续，不做 compaction |
| Dma32不是静态 reserve | 避免无需求板浪费低地址内存，关键设备应预分配 |
| 不预置 EmergencyReserve | 没有经过审计的保证进展消费者时不增加公共 API 和静态页 |
| DMA domain 当前为 identity | 当前平台是 IOMMU bypass；不得把 domain id 当成真实隔离 |
| fault最多一次 clean reclaim | 延迟有界，不在 allocator内做 I/O |
| 无 swap/NUMA/page migration/multi-gen LRU | 不符合当前嵌入式范围与复杂度预算 |
| fixed-capacity boot maps | early boot无堆且失败边界明确 |
| RT/IRQ使用专用固定池 | 保证关键路径不进入通用 allocator |

增加复杂机制前必须给出目标板 workload、绝对延迟/容量预算、采样证据和裁剪方案；仅以“Linux有该功能”不能作为引入理由。
