---
sidebar_position: 99
sidebar_label: "测试与验收"
---

# 内存管理测试与验收

内存修改必须同时验证区间事实、资源所有权、失败回滚、跨 CPU 可见性和热路径延迟。测试优先使用可控分配器、页表、后端和记录型 DMA 适配器，板级测试负责固件内存图、缓存、Translation Lookaside Buffer（地址转换后备缓冲区，TLB）与设备行为。

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
| x86_64 同时存在低端与更大的高端 RAM | early arena 位于 4 GiB 以下；高端 RAM 仍交给 Buddy |
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

分配失败不得触发 callback、虚拟文件系统、阻塞或隐式 retry。使用 fake reclaim counter 可以证明 `ax-alloc` 完全不调用上层回收。

## 2. 页表与地址空间测试

页表测试验证单个机制，地址空间测试验证多个机制组合后的 all-or-rollback。两者不能互相替代。

### 2.1 页表能力

每个架构 entry 应做 flags/页表项 round-trip，每个 engine 应做 map/query/protect/unmap 和 ownership teardown。

| 用例 | 断言 |
| --- | --- |
| entry round-trip | 物理地址、permission、device/uncached 与 huge bit 不丢失 |
| AArch64 MAIR layout | boot/EL1/EL2/页表项共用同一 index/value |
| frame allocation failure | 返回 `PagingError::NoMemory`，不留下 half-linked table |
| map conflict | 旧页表项保持不变 |
| huge mapping | alignment、level 和 translate offset正确 |
| Guest huge Linear clear | 按实际 block 数保存恢复项；clear 成功且不退化为逐 4 KiB 快照 |
| 混合页尺寸 range | 未对齐首尾使用 4 KiB，中间在允许时升级为 2 MiB/1 GiB |
| 属性边界 | 大页不跨越权限、缓存属性或所有权发生变化的边界 |
| cursor batching | 1..32 地址逐项 flush，超过阈值 full flush |
| 多核 validation | local-only + multi CPU + no 处理器间中断 初始化失败 |
| owned table Drop | 每个 owned child frame只释放一次 |

Stage-1、Stage-2 和 boot 需要分别构建，证明 feature 之间没有隐式调用。boot provider 的 no-free 语义应单独测试，不能用 runtime provider 的 Drop 预期套用。

### 2.2 虚拟区域与 backend 操作

`ax-memory-set` 测试覆盖重叠检测、split/shrink、直接 map/unmap/protect 和 metadata-only 操作。公共层没有 prepare/commit/rollback 状态机，不能保留针对已删除协议的 fault backend。

| 故障点 | 成功标准 |
| --- | --- |
| 重叠 map | `unmap_overlap = false` 返回 `AlreadyExists`，旧区域不变 |
| area split/shrink | 地址、actual/reported flags 和 backend offset 一致 |
| metadata-only move | 不调用 backend，不重复释放物理页 |
| 12 GiB Linear map | 不建立与 3,145,728 个基础页成比例的软件快照 |
| 分配型 map 中间失败 | backend 删除本次已安装前缀并释放对应 frame |
| 分配型 unmap | 每个被删除的 allocation-backed frame 恰好释放一次 |
| 跨多区域 backend 失败 | 明确验证直接语义，不声称公共层自动恢复前缀 |

ArceOS、Starry 和 axaddrspace backend 各自增加 frame、常驻内存集大小/写时复制或 Guest RAM ownership 断言。Starry 的 clone、连续填页和 mremap 使用专用恢复测试，不把 Linux 策略下沉为所有消费者共同承担的状态机。

## 3. Starry、DMA 与 MMIO 测试

Starry、DMA 和内存映射输入输出都涉及跨对象生命周期，必须覆盖 close、unmap、fork、timeout 和驱动初始化失败等非正常顺序。

### 3.1 Starry 虚拟内存

Starry 测试要同时比较用户可见结果和内部计数。涉及 syscall 语义时，测试期望以 Linux/POSIX 行为为基准。

| 用例 | 断言 |
| --- | --- |
| RLIMIT_AS replacement | 只计算 retained + requested，超限不改虚拟内存区域 |
| Always overcommit | mode=1，超 commit limit 仍可 reserve并准确报告 |
| Strict overcommit | mode=2，超 limit 返回 ENOMEM 路径 |
| commit 分类 | private anonymous/private file 仅 writable 计入；shared-anonymous owner 计入一次；file/device/imported memory 不计入 |
| fork 写时复制 overflow | checked `u32` 拒绝且 count不变 |
| fork 中间失败 | parent flags/refs/常驻内存集大小与 child resources回滚 |
| private file read→write | 常驻内存集大小 File 迁移到 Anon |
| mremap move | 只移动页表项；物理页所有权和聚合计数保持不变 |
| fault clean reclaim | 只对 内存不足回收一次、最多 retry一次 |
| proc status/statm | 虚拟内存大小、常驻内存集大小 categories、peak和 stack分类一致 |

Starry 直接发现的 QEMU/board case 应覆盖多线程 fault、fork/exec/exit、memfd/shared mapping、MAP_FIXED 和跨虚拟内存区域 `mprotect`。重型内存压力负载放在 `apps/starry`，通过 `cargo xtask starry app` 执行。

### 3.2 DMA 所有权

DMA host test 使用 tracking `DmaOp` 记录 allocation、free、map、unmap和 cache sync。硬件测试验证实际 cache和设备 completion。

| 用例 | 断言 |
| --- | --- |
| invalid `DmaPod` compile-fail | reference/resource类型无法进入 typed buffer |
| token Copy/Clone compile-fail | free/unmap token不可复制 |
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

## 4. 验证命令

内存 crate 的 host test可以使用 Cargo；ArceOS、StarryOS和 Axvisor 的系统构建/运行应使用 `cargo xtask`。文档改动另外执行 Docusaurus build。

### 4.1 单组件验证

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

修改 `ax-page-table` 时要分别验证 `stage1`、`stage2`、`boot` feature 组合；修改 `ax-alloc` 时覆盖实际存在的 `global-allocator`、`tracking` 和 `smp` 组合。hard-实时 与 reserve 尚不是 Cargo feature，只有增加真实消费者和构建配置后才加入对应矩阵。

### 4.2 工作区与系统验证

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

性能基线必须使用相同平台、CPU数、内存图、feature和 workload。平均值不能替代 P99/max，因为 实时和中断请求路径关注最坏延迟。

### 5.1 分配器指标

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
| 页表 | map/unmap/protect latency、各页尺寸叶子项数量、分配的 table frames、地址转换后备缓冲区 address/full flush次数 |
| 地址空间操作 | 单个区域操作耗时、backend 临时字节数、最大范围、失败前已处理页数 |
| Starry | minor fault P99/max、写时复制 copy、reclaim pages/attempts、常驻内存集大小 drift、fork/mremap latency |
| Guest | 嵌套页表 fault、Guest RAM populate/teardown、huge mapping比例 |
| DMA | alloc/map/unmap latency、cache sync bytes、bounce次数/bytes、quarantine/leak |
| hard-实时 | critical section通用 heap/page allocation次数 |
| boot | memory map处理时间、early bump bytes、per-CPU固定开销 |

hard-实时 的关键验收是已识别 实时 critical section 的通用堆和页分配次数为 0。驱动 ring/descriptor 应在 probe 或启动期预分配。

## 6. 静态架构检查

代码通过测试并不证明没有重复入口或反向依赖。每次修改内存子系统都要用 source/dependency scan 检查组件边界。

### 6.1 边界一致性

生产依赖必须保持单一入口：每个资源流只通过一个公共 crate 暴露能力，不得通过 re-export、alias 或绕过 facade 创建第二入口。

| 检查项 | 失败示例 |
| --- | --- |
| 第二 allocator 入口 | driver 直接依赖 `buddy-slab-allocator`，绕过 `ax-alloc` |
| compatibility re-export | 公共 crate 通过 module/type alias 重新暴露底层实现 |
| reverse dependency | `ax-page-table` 依赖 `ax-alloc` |
| duplicate stats | proc/kernel 维护另一套 allocator usage counters |
| bypassed DMA token | 地址/页数/bool 分离传参，或 token 实现 `Copy`/`Clone` |

Cargo.lock 冲突不手工合并；依赖冲突解决后由 Cargo 重新生成并检查 diff。

### 6.2 功能裁剪

分别生成最小 ArceOS、多核 ArceOS、Starry和 Axvisor dependency tree，确认执行模块只在需要时链接。

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
| 单全局 Buddy锁 | 简单、metadata少；测量触发前不加完整每处理器页缓存 |
| 连续 allocation不跨 section | 保证真实物理连续，不做 compaction |
| Dma32不是静态 reserve | 避免无需求板浪费低地址内存，关键设备应预分配 |
| 不预置 EmergencyReserve | 没有经过审计的保证进展消费者时不增加公共 API 和静态页 |
| DMA domain 当前为 identity | 当前平台是输入输出内存管理单元 bypass；不得把 domain id 当成真实隔离 |
| fault最多一次 clean reclaim | 延迟有界，不在 allocator内做 I/O |
| 无 swap/非统一内存访问/page migration/multi-gen 最近最少使用 | 不符合当前嵌入式范围与复杂度预算 |
| fixed-capacity boot maps | early boot无堆且失败边界明确 |
| 实时/中断请求使用专用固定池 | 保证关键路径不进入通用 allocator |

增加复杂机制前必须给出目标板 workload、绝对延迟/容量预算、采样证据和裁剪方案；仅以“Linux有该功能”不能作为引入理由。

## 8. 可复现实例

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

随后使用容量更小的 map制造 split capacity failure，并在调用前保存 clone；返回 `MemoryRangeError::Capacity` 后断言 `map == before`。这同时验证 transactional clone和无部分发布。

### 8.2 地址空间确定性用例

`memory/memory_set/src/tests.rs` 的 mock backend 记录直接 map/unmap/protect 调用。测试输入包含两个不连续区域和横跨边界的操作，用于核对 split/shrink 后的区域集合与 backend 调用范围。

```text
initial VMAs: [0x1000,0x4000), [0x5000,0x8000)
initial PTEs: pages 1,2,3,5,6,7 mapped
operation:    unmap [0x2000,0x7000)
operation:    unmap [0x2000,0x7000)

required result on success:
remaining VMAs: [0x1000,0x2000), [0x7000,0x8000)
backend receives only the intersecting subranges
no operation-sized snapshot or plan allocation
```

针对 metadata split，使用不能 split 的 backend 替换虚拟内存区域中间一页时应在修改前返回 `BadState`。metadata-only 操作必须证明不会调用页表 backend；它们只供已经在策略层完成页表移动的路径使用。

### 8.3 写时复制失败用例

Starry kernel axtest `cow_fault_accounting_failure_rolls_back` 构造两页连续填页请求，并把匿名页聚合计数预置为 `u64::MAX - 1`。第一页递增成功，第二页因 checked overflow 失败，从而确定性覆盖当前页和前序页的回滚。

| 检查对象 | 失败前中间状态 | 返回后期望 |
| --- | --- | --- |
| 第一页页表项 | 已 map | 不存在 |
| 匿名页聚合计数 | 从 `u64::MAX - 1` 增至 `u64::MAX` | 恢复为 `u64::MAX - 1` |
| 第一页 frame | live | 已归还 |
| 第二页页表项 | 已 map，随后计数溢出 | 不存在 |
| 返回值 | 计数溢出 | `BadState` |

该用例必须通过 `cargo xtask ktest qemu` 运行，因为它需要真实 kernel PageTable与 frame table；host-only `starry-mm` 测试无法覆盖页表项/frame回滚。

```sh
cargo xtask ktest qemu \
  -p starry-kernel \
  --test axtest_kernel \
  --arch x86_64
```

测试输出必须包含对应 case 的 `ok` 和最终 `AXTEST_SUITE_OK`。QEMU启动成功但 case未被发现不算通过。

### 8.4 分配器压力用例

`memory/buddy-slab-allocator/tests/stress_test.rs` 的 ignored tests需要显式执行，并启用单 test thread，避免全局 allocator singleton用例互相干扰。

```sh
cargo test -p buddy-slab-allocator \
  --test stress_test \
  -- --ignored --test-threads=1
```

九个用例覆盖多 section、exhaustion recovery、fragmentation、multi-thread page allocation、mixed small/large allocation和 remote free。host build必须通过 `ax-kspin/host-test` 使用 no-op 中断请求 backend；否则 `SpinNoIrq` 在用户态执行 x86 `cli/sti` 会以 SIGSEGV终止，这属于测试配置错误而非 production lock降级理由。

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

对比基线与新实现时必须使用相同固件内存图和 CPU frequency policy。若 P99退化超过 10%，先用 lock wait、地址转换后备缓冲区 flush或 reclaim次数定位来源；不能直接用平均吞吐掩盖尾延迟。

### 8.6 大范围映射回归

大范围回归使用一个虚拟地址与物理地址均按 1 GiB 对齐的 12 GiB Linear 区间。测试页表 provider 应记录页表 frame 分配次数，并允许在指定次数返回 `None`；backend 还应暴露仅供测试读取的 plan 保存项数量。

```text
range:             0x4000_0000..0x3_4000_0000
size:              12 GiB
base pages:        3,145,728
Map previous.len:  0
prepare allocation: independent of base-page count
```

该地址示例的末端由受检加法计算，测试代码不能直接信任文本常量。测试分三组运行，分别证明准备阶段、提交失败和页尺寸策略。

| 组别 | 注入或配置 | 必须断言 |
| --- | --- | --- |
| Map prepare | 只创建 plan，不提交 | `previous.len() == 0`，没有约 96 MiB 快照分配 |
| 4 KiB commit failure | `allow_huge=false`，在中间页表 frame 分配失败 | 返回 `NoMemory`；查询整个已完成前缀均为未映射；虚拟内存区域未发布 |
| 大页 capability | `allow_huge=true`，范围属性一致 | 生成 12 个 1 GiB 叶子项，不分配 4 KiB 叶子表 |
| 属性边界 | 中间插入 2 MiB 设备或只读区 | 请求先按属性拆分；任何大页都不跨边界 |
| Unmap/Protect capacity | 强制 `previous.try_reserve()` 失败 | 在修改页表前返回 `NoMemory`，原范围逐点抽样和边界查询均一致 |

系统级 QEMU 回归还要使用真实固件内存清单复现原始 12 GiB 保留区。日志至少记录该区间的 `MemoryType`、是否进入 `ax-hal::memory_regions()`、是否进入 `new_kernel_aspace()`、实际页尺寸计数和页表 frame 总量。若区间属于固件私有窗口，正确结果是排除分配且不建立普通直接映射；若属于必须访问的保留 RAM，才比较大页与基础页映射成本。
