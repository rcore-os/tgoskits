---
sidebar_position: 99
sidebar_label: "测试"
---

# 内存管理测试

内存测试覆盖区间事实、资源所有权、失败回滚、跨 CPU 可见性和热路径延迟。Host 测试使用可控分配器、页表、后端和记录型 DMA 适配器；板级测试覆盖固件内存图、缓存、Translation Lookaside Buffer（地址转换后备缓冲区，TLB）与设备行为。

## 1. 启动与 allocator 测试

启动内存和 allocator 的错误会污染所有上层测试，因此需要先验证物理总量和不重叠，再测试分配行为。

### 1.1 启动内存图

确定性输入应包含至少两段 RAM、一个跨 Free 中部的 reservation、KImage、early bump used prefix 和 MMIO hole。输出逐段比较，而不是只比较总大小。

| 用例 | 断言 |
| --- | --- |
| Free 中插入 Reserved | Free 被 split，Reserved 精确保留 |
| 相邻同类型 reservation | 合并为单一描述符 |
| 不同 non-Free overlap | 返回 `RangeError::Conflict`，新描述符未加入 |
| fixed capacity exhausted | 返回 `RangeError::Capacity`；注意 merge_add 原地修改，此前拆分不回滚，启动路径应立即 panic |
| range/alignment overflow | 返回 typed error，无 wrapping range |
| 多 memory nodes/regions | 每个合法 bank 都进入最终 Free 列表 |
| early bump 交接 | `memory_map_setup()` 把 used prefix 变 Reserved；`memories()` 超过 128 ranges 时 `.ok()` 静默截断应被测试暴露 |
| 多段 RAM 与首个大段 | early arena 是排序后第一个大于 8 MiB 的 Free 段（与架构、地址高低无关）；无候选时启动失败 |
| 12 GiB 固件保留区 | 启动内存图只增加一个区间描述符，不按 4 KiB 展开 |
| RAM、MMIO 与地址空洞分类 | 只有 Free RAM 进入 Buddy；设备区使用设备属性；不可访问区不误作普通 RAM |

板级启动日志应保存固件输入、someboot memory map、ax-hal normalized regions和 Buddy managed sections，逐层核对丢失的 bytes 属于哪类 metadata或对齐。

### 1.2 页与小对象分配

host test 使用可控内存 slice 建立多个 section，覆盖 page、lowmem、small object、large object 和 remote-free。

| 用例 | 断言 |
| --- | --- |
| add disjoint regions | section count 和 managed bytes正确 |
| add overlap | 返回 `MemoryOverlap` |
| too-small added region | 明确 skip，不产生无效 section |
| Normal page | size/alignment 满足，free 后可合并 |
| Dma32 page | allocation 最后一个 byte 小于 4 GiB |
| large contiguous | 不跨 section；无单 section 可满足时 内存不足 |
| all size classes | round-up、bitmap 和 empty-slab return正确 |
| cross-CPU free | remote stack只消费一次，owner drain 后可复用 |

字节分配失败不触发 callback、虚拟文件系统、阻塞或隐式 retry。显式页分配（`alloc_pages()` / `alloc_dma32_pages()`）失败则会在释放 allocator 锁后调用已注册的 page reclaim callback 并最多重试 4 轮；使用 fake reclaim counter 可以验证“锁外执行、重试有界、回收 0 页即返回 `NoMemory`”。

## 2. 页表与地址空间测试

页表测试验证递归映射、页表帧和失效机制，地址空间测试验证区域元数据与具体 backend 的组合语义。不同 backend 的回滚边界并不相同，因此测试必须分别固定 ArceOS、StarryOS 和 Axvisor 的实际资源结果，不能用一个笼统的 all-or-rollback 预期替代。

### 2.1 页表能力

每个架构 entry 应做 flags/页表项 round-trip，每个 engine 应做 map/query/protect/unmap 和 ownership teardown。

| 用例 | 断言 |
| --- | --- |
| entry round-trip | 物理地址、permission、device/uncached 与 huge bit 不丢失 |
| AArch64 MAIR layout | 运行时 3 slot（`0x44ff04`）与 boot 4 slot 布局中 index 0/1/2 语义一致 |
| frame allocation failure | 返回 `PagingError::NoMemory`，不留下 half-linked table |
| map conflict | 旧页表项保持不变 |
| huge mapping | alignment、level 和 translate offset正确 |
| Guest huge Linear clear | 按实际 block 数保存恢复项；clear 成功且不退化为逐 4 KiB 快照 |
| 混合页尺寸 range | 未对齐首尾使用 4 KiB，中间在允许时升级为 2 MiB/1 GiB |
| 属性边界 | 大页不跨越权限、缓存属性或所有权发生变化的边界 |
| map_region 批量失效 | ≤ `TARGETED_FLUSH_LIMIT`（32）个新映射逐地址 flush，超过阈值一次 full flush；protect/remap 立即逐地址 flush |
| 多核 shootdown | 解除共享内核映射走 `flush_tlb_range_all_cpus()`，未 ready/offline CPU 被跳过（`CpuOffline`） |
| owned table Drop | 每个 owned child frame只释放一次，Drop 不隐式 flush |

主机第一阶段、客户机第二阶段和 boot 页表需要分别构建，证明所属 crate 之间没有错误耦合。boot provider 的 no-free 语义应单独测试，不能用 runtime provider 的 Drop 预期套用。

### 2.2 虚拟区域与 backend 操作

`ax-memory-set` 测试覆盖重叠检测、split/shrink、直接 map/unmap/protect 和 metadata-only 操作。公共层没有 prepare/commit/rollback 状态机，不能保留针对已删除协议的 fault backend。

| 故障点 | 成功标准 |
| --- | --- |
| 重叠 map | `unmap_overlap = false` 返回 `AlreadyExists`，旧区域不变 |
| area split/shrink | 地址、actual/reported flags 和 backend offset 一致 |
| metadata-only move | 不调用 backend，不重复释放物理页 |
| 12 GiB Linear map | 不建立与 3,145,728 个基础页成比例的软件快照 |
| ArceOS 分配型 map 中间失败 | `populate_pages()` 回滚当前操作已安装的全部前缀页，并归还当前尚未映射及已经解除映射的 frame |
| Axvisor 分配型 map 中间失败 | 固定当前遗留语义：`axaddrspace` 尚不回滚已安装前缀，失败页的 frame 也可能泄漏；测试应准确暴露该差异 |
| 分配型 unmap | 每个被删除的 allocation-backed frame 恰好释放一次；axaddrspace 遇到大页首项时先删项后失败的行为需单独断言 |
| 跨多区域 backend 失败 | 明确验证直接语义，不声称公共层自动恢复前缀 |

ArceOS、Starry 和 axaddrspace backend 各自增加 frame、常驻内存集大小/写时复制或 Guest RAM ownership 断言。Starry 的 clone、连续填页和 mremap 使用专用恢复测试，不把 Linux 策略下沉为所有消费者共同承担的状态机。

## 3. Starry、DMA 与 MMIO 测试

Starry、DMA 和内存映射输入输出都涉及跨对象生命周期，必须覆盖 close、unmap、fork、timeout 和驱动初始化失败等非正常顺序。

### 3.1 Starry 虚拟内存

Starry 测试要同时比较用户可见结果和内部计数。涉及 syscall 语义时，测试期望以 Linux/POSIX 行为为基准。

| 用例 | 断言 |
| --- | --- |
| RLIMIT_AS replacement | 只计算 retained + requested，超限不改虚拟内存区域 |
| overcommit proc 展示 | 当前 procfs 展示 overcommit 控制项，`/proc/meminfo` 的 `Committed_AS` 仍为 0 |
| fork 写时复制 overflow | 当前 `u8` 引用计数达到上限时不能静默回绕 |
| fork 中间失败 | parent flags/refs/常驻内存集大小与 child resources回滚 |
| private file read→write | 常驻内存集大小 File 迁移到 Anon |
| mremap move | 只移动页表项；物理页所有权和聚合计数保持不变 |
| allocator page-cache reclaim | page allocation 失败后锁外调用注册 reclaim，最多 4 轮重试 |
| proc status/statm | 虚拟内存大小、常驻内存集大小 categories、peak和 stack分类一致 |

Starry 直接发现的 QEMU/board case 应覆盖多线程 fault、fork/exec/exit、memfd/shared mapping、MAP_FIXED 和跨虚拟内存区域 `mprotect`。重型内存压力负载放在 `apps/starry`，通过 `cargo xtask starry app` 执行。

### 3.2 DMA 所有权

DMA host test 使用 tracking `DmaOp` 记录 allocation、free、map、unmap和 cache sync。硬件测试验证实际 cache和设备 completion。

| 用例 | 断言 |
| --- | --- |
| DMA descriptor 使用点审计 | 当前 `DmaPod` blanket impl 覆盖所有 `Copy`，测试和评审需验证 descriptor 真正 plain-data |
| handle Copy/Clone 风险 | 当前底层 handle 可复制，高层 owner 仍应只释放一次 |
| 资源获取即初始化 Drop | coherent、contiguous、streaming 各释放一次 |
| backend violates mask | token先被释放，再返回 typed error |
| bounce direction | copy-in/out 和 clean/invalidate 顺序正确 |
| fd close before mmap unmap | dma-buf backing由 mmap retainer保留 |
| accelerator import | operation结束前 owner live，driver不释放 |
| reset/timeout | quiesce 后 complete；无法证明时 quarantine/leak |

`qemu-rga/system/rga-lifecycle` 和 dma-buf 最后引用释放用例应作为设备路径回归；物理板还要验证 JPEG/NPU/TPU 使用同一 Dma32 ownership模型。

### 3.3 MMIO 映射与寄存器访问

MMIO host test 使用记录型 `MmioOp` 统计映射和解除映射，平台测试核对实际页表属性、访问宽度和硬件要求的屏障顺序。

| 用例 | 断言 |
| --- | --- |
| 零长度和末地址溢出 | 建立页表映射前返回明确错误 |
| 未初始化 capability | 第一次映射时确定性失败，不使用空后端 |
| 未对齐物理窗口 | 页对齐映射覆盖完整范围并恢复页内偏移 |
| `Mmio` Drop | 一个映射 owner 只调用一次 `iounmap()` |
| `MmioRaw` clone | 借用对象不被误认为独立映射 owner |
| 末端固定宽度访问 | `offset + width` 不超过映射长度 |
| 页表属性 | 设备窗口使用设备属性，不使用普通缓存属性 |
| 驱动初始化中途失败 | 已构造 owner 按析构顺序解除映射 |
| 寄存器语义 | 只读、写一清零、doorbell 和 read-back 顺序符合设备规范 |

测试不得用普通切片访问代替易失性寄存器读写。易失性只能约束编译器访问，设备协议要求的 CPU 屏障仍需由驱动或平台实现并在目标架构上验证。

## 4. 测试命令

内存 crate 的 host test可以使用 Cargo；ArceOS、StarryOS和 Axvisor 的系统构建/运行应使用 `cargo xtask`。文档改动另外执行 Docusaurus build。

### 4.1 组件测试

修改单一 crate 时先运行格式、该 crate clippy和对应 unit/doc tests。以下命令是常用最小集合，feature 应按改动补齐。

```sh
cargo fmt --all --check
cargo xtask clippy --package ax-alloc
cargo xtask clippy --package page-table-generic
cargo xtask clippy --package ax-cpu
cargo xtask clippy --package ax-memory-set
cargo xtask clippy --package dma-api
cargo test -p ax-memory-set
cargo test -p dma-api
```

修改公共 walker 时运行 `page-table-generic` 测试；修改主机页表时按目标架构验证 `ax-cpu`；修改第二阶段或启动页表时分别验证 `axvm` 或 `someboot`。修改 `ax-alloc` 时覆盖实际存在的 feature 组合：`global-allocator`、`tlsf`、`buddy-slab` 与 `tracking`。hard-实时 与 reserve 尚不是 Cargo feature，只有增加真实消费者和构建配置后才加入对应矩阵。

### 4.2 工作区与系统测试

依赖或 feature 改动需要检查 workspace metadata 和生产 dependency tree。系统命令以仓库 `cargo xtask --help` 和现有 CI配置为准。

```sh
cargo metadata --format-version 1
cargo tree --workspace
```

ArceOS、StarryOS 和 Axvisor 至少各选择一个 paging 配置构建；Starry 另运行直接发现的内存相关 QEMU case，重型压力负载通过 `cargo xtask starry app` 执行。物理 board、自托管 runner 和设备压力测试按变更范围执行。

## 5. 性能与容量观测

性能基线必须使用相同平台、CPU数、内存图、feature和 workload。平均值不能替代 P99/max，因为 实时和中断请求路径关注最坏延迟。

### 5.1 分配器指标

allocator benchmark 分开记录 Slab、Buddy order-0、高阶连续页、Dma32和 cross-CPU free。统计至少包括延迟、锁等待和空间开销。

| 指标 | 采集维度 | 用途 |
| --- | --- | --- |
| alloc/free latency | median、P99、max | 与相同配置的既有基线比较 |
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
| 页表 | map/unmap/protect latency、各页尺寸叶子项数量、分配的 table frames、地址转换后备缓冲区 address/full flush次数 |
| 地址空间操作 | 单个区域操作耗时、backend 临时字节数、最大范围、失败前已处理页数 |
| Starry | minor fault P99/max、写时复制 copy、reclaim pages/attempts、常驻内存集大小 drift、fork/mremap latency |
| Guest | 嵌套页表 fault、Guest RAM populate/teardown、huge mapping比例 |
| DMA | alloc/map/unmap latency、cache sync bytes、bounce次数/bytes、quarantine/leak |
| hard-实时 | critical section通用 heap/page allocation次数 |
| boot | memory map处理时间、early bump bytes、per-CPU固定开销 |

已识别的实时 critical section 需要记录通用堆和页分配次数。驱动 ring/descriptor 在 probe 或启动期预分配，避免把通用分配器引入实时路径。

## 6. 依赖边界

组件边界同时受源码依赖和 Cargo dependency tree 约束。生产路径不能保留重复入口或反向依赖。

### 6.1 边界一致性

生产依赖必须保持单一入口：每个资源流只通过一个公共 crate 暴露能力，不得通过 re-export、alias 或绕过 facade 创建第二入口。

| 检查项 | 失败示例 |
| --- | --- |
| 第二 allocator 入口 | driver 直接依赖 `buddy-slab-allocator`，绕过 `ax-alloc` |
| compatibility re-export | 公共 crate 通过 module/type alias 重新暴露底层实现 |
| reverse dependency | `page-table-generic` 或 `ax-cpu` 依赖 `ax-alloc` |
| duplicate stats | proc/kernel 维护另一套 allocator usage counters |
| bypassed DMA token | 地址/页数/bool 分离传参，或 token 实现 `Copy`/`Clone` |

Cargo.lock 冲突不手工合并；依赖冲突解决后由 Cargo 重新生成并检查 diff。

### 6.2 功能裁剪

分别生成最小 ArceOS、多核 ArceOS、Starry和 Axvisor dependency tree，确认执行模块只在需要时链接。

| 构建 | 应存在 | 不应存在 |
| --- | --- | --- |
| embedded ArceOS | `ax-alloc`、必要 Stage-1 | Starry policy、Stage-2、unused reserve |
| Starry | Stage-1、Starry kernel mm、Linux backend | Stage-2 |
| Axvisor | Stage-2、`axaddrspace` | Starry policy、boot engine runtime copy |
| boot-only platform component | `page-table-generic` | runtime allocator依赖循环 |

feature scan还要比较静态符号和镜像大小，避免关闭 feature 后只隐藏 API但仍保留全局状态。

## 7. 当前设计约束

当前设计约束来自嵌入式容量、尾延迟和实现复杂度的共同取舍，不应被当成缺失功能自动补齐。下表同时说明每项限制保护的边界，新增机制前需要先提供真实消费者和可复现测量证据。

| 约束 | 理由 |
| --- | --- |
| 单全局 Buddy锁 | 简单、metadata少；测量触发前不加完整每处理器页缓存 |
| 连续 allocation不跨 section | 保证真实物理连续，不做 compaction |
| Dma32不是静态 reserve | 避免无需求板浪费低地址内存，关键设备应预分配 |
| 不预置 EmergencyReserve | 没有经过审计的保证进展消费者时不增加公共 API 和静态页 |
| DMA domain 当前为 identity | 当前平台是输入输出内存管理单元 bypass；不得把 domain id 当成真实隔离 |
| allocator reclaim 重试有界 | 延迟有界，不在 allocator 锁内做 I/O |
| 无 swap/非统一内存访问/page migration/multi-gen 最近最少使用 | 不符合当前嵌入式范围与复杂度预算 |
| fixed-capacity boot maps | early boot无堆且失败边界明确 |
| 实时/中断请求使用专用固定池 | 保证关键路径不进入通用 allocator |

增加复杂机制前必须给出目标板 workload、绝对延迟/容量预算、采样证据和裁剪方案；仅以“Linux有该功能”不能作为引入理由。

## 8. 测试场景

内存测试必须给出确定输入、故障位置和完整状态断言。只执行压力负载或只断言返回 `Err` 无法证明 ownership 与专用失败清理正确。

### 8.1 启动内存用例

构造一个容量足够的 `heapless::Vec<MemoryDescriptor, N>`，先加入 `Free [0x1000,0x9000)`，再加入 `Reserved [0x3000,0x5000)`。测试同时断言结果顺序、类型和半开端点。

```rust
let mut map = heapless::Vec::<MemoryDescriptor, 8>::new();
map.push(MemoryDescriptor::new_with_range(
    0x1000..0x9000,
    MemoryType::Free,
))
.unwrap();
map.merge_add(MemoryDescriptor::new_with_range(
    0x3000..0x5000,
    MemoryType::Reserved,
))
.unwrap();
```

期望输出必须是三段，不允许只比较总大小 32 KiB。

| Index | Range | Type |
| ---: | --- | --- |
| 0 | `0x1000..0x3000` | Free |
| 1 | `0x3000..0x5000` | Reserved |
| 2 | `0x5000..0x9000` | Free |

随后使用容量更小的 map制造 split capacity failure：`merge_add()` 返回 `RangeError::Capacity`。注意当前实现原地修改，此前已完成的拆分不会回滚；启动路径因此对错误一律 `unwrap`/panic。测试应断言错误返回本身，不能断言 `map == before` 这种事务性行为。

### 8.2 地址空间确定性用例

`memory/memory_set/src/tests.rs` 的 mock backend 记录直接 map/unmap/protect 调用。测试输入包含两个不连续区域和横跨边界的操作，用于核对 split/shrink 后的区域集合与 backend 调用范围。

```text
initial VMAs: [0x1000,0x4000), [0x5000,0x8000)
initial PTEs: pages 1,2,3,5,6,7 mapped
operation:    unmap [0x2000,0x7000)

required result on success:
remaining VMAs: [0x1000,0x2000), [0x7000,0x8000)
backend receives only the intersecting subranges
no operation-sized snapshot or plan allocation
```

针对 metadata split，使用不能 split 的 backend 替换虚拟内存区域中间一页时应在修改前返回 `BadState`。metadata-only 操作必须证明不会调用页表 backend；它们只供已经在策略层完成页表移动的路径使用。

### 8.3 Starry MM 事务与 ownership 用例

Starry 没有独立 `starry-mm` crate；可纯状态验证的生命周期、receipt、rmap 和 tag allocator 测试位于 `starry-kernel` axtest，真实 PTE、fault 与 syscall 语义由 grouped QEMU system case 覆盖。每个故障注入都要在旧实现上确定性失败，再由同一用例证明修复。

| 检查对象 | 注入或交错 | 必须保持的不变量 |
| --- | --- | --- |
| `PreparedMutation` | reservation 或 commit 前失败 | VMA/PTE/slot/epoch 均不变化 |
| PTE apply | 中途失败 | preimage 逆序恢复；不能证明时进入 `NeedsRepair` |
| TLB retirement | 远端 unsupported、timeout 或延迟 ack | receipt 保持 pending，detached owner 留在 quarantine |
| MM lifecycle | 最后一个 `MmHandle` 释放但仍有 pin/activation | 页表不得 clear，不能产生 `RetirePermit` |
| COW/rmap | 父子交错写或 File→Anon | 只替换当前 slot，其他 rmap 仍指向原 `PageObject` |
| shared THP split | partial unmap/mprotect | child slot 共享原 `PageObject`，`frame_offset` 精确指向物理子范围 |
| resident stats | sparse map、unmap 和 reclaim | 当前 RSS 等于 published slot 汇总，VmHWM 单调 |

需要真实页表的 axtest 通过项目任务入口运行：

```sh
cargo xtask ktest qemu \
  -p starry-kernel \
  --test axtest_kernel \
  --arch x86_64
```

测试输出必须包含对应 case 的 `ok` 和最终 `AXTEST_SUITE_OK`。Linux ABI 用例通过 `cargo xtask starry test qemu --arch <arch> -c qemu/system/<case>` 运行，最终必须出现 `STARRY_GROUPED_TESTS_PASSED`；QEMU 启动成功、任务取消或 case 未被发现都不算通过。

### 8.4 分配器压力用例

`memory/buddy-slab-allocator/tests/stress_test.rs` 的 ignored tests需要显式执行，并启用单 test thread，避免全局 allocator singleton用例互相干扰。

```sh
cargo test -p buddy-slab-allocator \
  --test stress_test \
  -- --ignored --test-threads=1
```

九个用例覆盖多 section、exhaustion recovery、fragmentation、multi-thread page allocation、mixed small/large allocation和 remote free。host build必须通过 `ax-sync` 的 `host-test` feature（或 `cfg(test)` 内建 host provider）使用 no-op 中断请求 backend；否则 `lock_irqsave()` 在用户态执行 x86 `cli/sti` 会以 SIGSEGV终止，这属于测试配置错误而非 production lock降级理由。

### 8.5 性能样本格式

性能结果应保存 workload参数和原始分位数，不能只写“无明显退化”。下面给出一条 allocator样本应包含的最小字段。

| 字段 | 示例 |
| --- | --- |
| board/CPU | `orangepi-5-plus / 8 cores` |
| build | commit、target、release、feature集合 |
| memory map | Free sections和 managed bytes |
| operation | order-0 alloc/free、64 B Slab、16-page contiguous等 |
| concurrency | CPU数、每 CPU线程/循环数 |
| samples | warmup次数、有效样本数 |
| latency | median、P95、P99、max，统一单位 |
| contention | Buddy lock wait和 remote-free drain |
| capacity | free pages、largest block、metadata bytes |

对比基线与新实现时使用相同固件内存图和 CPU frequency policy。出现明显尾延迟变化时，使用 lock wait、地址转换后备缓冲区 flush 或 reclaim 次数定位来源；平均吞吐不能替代分位数。

### 8.6 大范围映射回归

大范围回归使用一个虚拟地址与物理地址均按 1 GiB 对齐的 12 GiB Linear 区间。当前实现没有 `MappingPlan` 或 `previous` 快照，测试应直接记录 `MemorySet` 区域数量、页表 frame 分配次数、叶子项尺寸和失败后的查询结果。

```text
range:             0x4000_0000..0x3_4000_0000
size:              12 GiB
base pages:        3,145,728
MemoryArea count:  1
software undo log: none
```

该地址示例的末端由受检加法计算，测试代码不能直接信任文本常量。测试分别证明区域元数据保持常数规模、`map_region()` 失败回滚已建立叶子项，以及大页选择不跨越属性边界。

| 组别 | 注入或配置 | 必须断言 |
| --- | --- | --- |
| 区域元数据 | 构造一个 12 GiB `MemoryArea` | `MemorySet` 只增加一个区域，不产生与基础页数量成比例的撤销数组 |
| 4 KiB map failure | `allow_huge=false`，让 frame provider 在中间下级页表分配时返回 `None` | `map_region()` 返回 `NoMemory` 并解除当前调用已经建立的前缀；上层 `MemoryArea` 不发布 |
| 大页 capability | `allow_huge=true`，范围属性一致 | 生成 12 个 1 GiB 叶子项，不分配 4 KiB 叶子表 |
| 属性边界 | 中间插入 2 MiB 设备或只读区 | 请求先按属性拆分；任何大页都不跨边界 |

这些断言分别对应 `memory/memory_set/src/set.rs` 和 `memory/page-table-generic/src/table.rs::map_region()` 的当前控制流。若测试仍引用 `MappingPlan`、`previous.len()` 或 `previous.try_reserve()`，它验证的是已经删除的实现模型，应当改写而不是保留兼容 fixture。

系统级 QEMU 回归还要使用真实固件内存清单复现原始 12 GiB 保留区。日志至少记录该区间的 `MemoryType`、是否进入 `ax-hal::memory_regions()`、是否进入 `new_kernel_aspace()`、实际页尺寸计数和页表 frame 总量。若区间属于固件私有窗口，正确结果是排除分配且不建立普通直接映射；若属于必须访问的保留 RAM，才比较大页与基础页映射成本。
