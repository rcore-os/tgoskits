# 内核功能开发准则

内核功能开发的目标不是“把路径跑通”，而是把启动顺序、资源所有权、并发上下文、硬件语义和用户可见 ABI 写清楚，并用可复现的验证证明这些约束没有被破坏。

本准则适用于 ArceOS、StarryOS、Axvisor、平台层、驱动层、内存管理、任务调度、文件系统、系统调用和虚拟化相关改动。

## 1. 总原则

| 原则 | 要求 |
|------|------|
| 先边界后实现 | 先确定能力属于 kernel core、组件、驱动、平台、OS glue 还是工具链 |
| 先所有权后共享 | 明确资源由谁创建、谁释放、谁能并发访问、谁负责错误恢复 |
| 先语义后适配 | 内核内部语义稳定后，再适配 Linux/POSIX/VirtIO/硬件寄存器或板卡差异 |
| 先最小路径后扩展 | 先实现一条可验证主路径，再扩展架构、平台、feature 和异常路径 |
| 先确定性验证后合入 | 每个风险点都应有本地命令、单元测试、QEMU 用例或板卡证据覆盖 |

功能实现前必须回答：

1. 这个能力属于哪个层次，是否放错了 crate 或模块？
2. 它新增了什么状态、资源、锁、IRQ、DMA、页表、进程或文件系统对象？
3. 谁拥有这些对象，生命周期在哪里开始和结束？
4. 错误发生时是否能显式返回、回滚、释放或标记为 unsupported？
5. 哪个测试能证明主路径和失败路径都按预期工作？

---

# 2. 启动与平台

## 2.1 启动代码必须保持阶段清楚

启动流程应按阶段组织，不要把固件解析、内存发现、平台设备探测、IRQ 初始化、timer 初始化、调度器启动和第一个任务创建堆在一个函数中。

推荐阶段：

| 阶段 | 关注点 |
|------|--------|
| 固件入口 | boot 参数、FDT/ACPI/UEFI/someboot 输入是否有效 |
| 早期内存 | 可用物理区间、保留区、内核镜像、boot stack、DMA 限制 |
| 早期输出 | console、日志级别、panic 输出路径 |
| 平台发现 | CPU、timer、interrupt controller、UART、block、net、virtio/mmio/pci |
| 子系统初始化 | allocator、paging、scheduler、driver registry、filesystem |
| 进入运行态 | idle task、init task、Starry/ArceOS/Axvisor 主入口 |

启动函数应像目录：

```rust
pub fn boot_kernel(params: BootParams) -> Result<(), BootError> {
    let firmware = parse_firmware_tables(params)?;
    let memory = initialize_early_memory(&firmware)?;
    let platform = discover_platform_devices(&firmware)?;

    initialize_console(&platform)?;
    initialize_interrupts(&platform)?;
    initialize_timer(&platform)?;
    initialize_runtime(memory, platform)?;

    start_first_task()
}
```

## 2.2 平台差异必须收敛到平台边界

架构和板卡差异应放在明确边界内，不应散落在业务逻辑中。

| 差异 | 推荐位置 |
|------|----------|
| 指令、寄存器、异常上下文 | `arch` / `axcpu` / 架构后端 |
| CPU bring-up、IPI、timer、interrupt controller | platform / somehal / someboot 边界 |
| FDT/ACPI/UEFI 解析 | firmware/platform adapter |
| 板卡设备地址、IRQ、clock、reset | board/platform 配置 |
| OS 用户态 ABI | Starry syscall/process 边界 |

禁止在通用组件中直接写板卡地址、magic IRQ、裸 `cfg(board)` 分支或临时 fallback。平台缺失能力时，应返回 typed unsupported 错误，或在配置层明确声明不支持。

## 2.3 多架构设计先抽公共语义，再实现单架构

可以先实现一个架构，但接口必须避免把该架构的偶然细节变成公共语义。

检查项：

- 接口名描述的是通用能力，还是某个架构寄存器名？
- 错误类型能否表达“当前架构不支持”？
- 是否把页表级数、异常编号、timer 频率、cache 行大小写死到通用层？
- 是否为后续 LoongArch、x86_64、aarch64、riscv64 留出后端实现空间？

---

# 3. 任务调度与同步

## 3.1 调度路径必须区分上下文

同一段逻辑在普通任务、内核线程、softirq、hard IRQ、scheduler、idle 和 panic 路径中的限制不同。新增调度功能前必须标注执行上下文。

| 上下文 | 允许 | 禁止或谨慎 |
|--------|------|------------|
| 普通任务 | 睡眠锁、阻塞等待、分配、文件系统访问 | 长时间持锁 |
| scheduler 路径 | 短临界区、明确状态转换 | 睡眠、复杂分配、回调外部代码 |
| hard IRQ | 非睡眠锁、ack/mask/unmask、记录事件 | 睡眠、阻塞 IO、宽锁、复杂日志 |
| idle | 低功耗等待、调度检查 | 依赖普通任务上下文 |
| panic | 最小输出、停止或重启 | 需要锁顺序和分配才能完成的清理 |

## 3.2 任务状态转换必须显式

任务状态不能靠多个布尔位隐式组合。优先用 enum、typed state 或小函数表达状态转换。

```rust
fn block_current(reason: BlockReason) -> Result<(), ScheduleError> {
    let task = take_current_task()?;
    let blocked = task.transition_to_blocked(reason)?;
    enqueue_blocked_task(blocked);
    schedule_next()
}
```

状态转换函数必须说明：

- 调用前任务处于什么状态；
- 调用后任务进入什么队列或生命周期；
- 失败时是否回滚；
- 是否允许在 IRQ 禁用、抢占禁用或持锁状态下调用。

## 3.3 锁和唤醒不能互相缠绕

新增同步逻辑时必须检查：

- 是否在持锁期间调用外部回调、wake、notify、poll 或用户提供函数；
- 是否存在锁顺序，需要在模块文档中说明；
- 是否把 IRQ-safe 锁和 sleepable 锁混用；
- 是否存在 lost wakeup，等待条件和唤醒条件是否来自同一状态源；
- 原子变量的 Acquire/Release 关系是否清楚。

---

# 4. 内存管理

## 4.1 内存对象必须有明确所有者

每类内存对象都应能回答“谁拥有、谁映射、谁释放、谁能 DMA、谁能给用户态看到”。

| 对象 | 典型所有者 | 关键约束 |
|------|------------|----------|
| 物理 frame | frame allocator / address space | 对齐、引用计数、保留区 |
| 虚拟映射 | address space / page table owner | 权限、生命周期、TLB flush |
| 用户内存 | process / user memory accessor | 边界检查、copy in/out、fault 处理 |
| DMA buffer | driver / DMA API | cache coherency、IOMMU、设备可见地址 |
| boot memory | platform / early allocator | 初始化后是否可回收 |

## 4.2 页表和地址空间修改必须可审计

页表相关函数必须暴露：

- 映射地址、大小、权限和属性；
- 是否覆盖已有映射；
- 失败时已安装的部分映射如何回滚；
- 是否需要 TLB shootdown；
- 是否影响用户态 ABI、设备 DMA 或虚拟机二阶段地址转换。

禁止把权限、cache 属性、用户/内核位和 device memory 属性混在裸 `usize` 中传递。优先使用 typed flags、newtype 和明确的 mapping request。

## 4.3 用户内存访问必须通过边界 API

系统调用、驱动和文件系统不能随意解引用用户指针。用户内存访问应集中到 user memory accessor 或 syscall 边界，并显式处理：

- 地址范围检查；
- 页故障和不可访问页；
- 字符串终止和最大长度；
- copy in/out 的部分成功；
- TOCTOU 风险；
- 与进程地址空间生命周期的关系。

---

# 5. 系统调用与进程

## 5.1 syscall 实现要分三层

推荐结构：

| 层次 | 职责 |
|------|------|
| ABI 层 | 解析 syscall 编号、寄存器参数、用户指针、返回值编码 |
| 语义层 | 实现 Linux/POSIX/Starry 语义，处理权限、状态和错误 |
| 资源层 | 操作进程、文件描述符、地址空间、线程、信号、文件系统对象 |

不要在 ABI 层直接完成复杂业务，也不要让资源层知道用户态寄存器布局。

## 5.2 错误码转换必须在边界完成

内部应使用 typed error，syscall 边界再转换成 Linux/POSIX errno 或 Starry 所需返回值。

```rust
fn sys_openat(args: OpenAtArgs) -> SyscallReturn {
    open_file(args)
        .map(SyscallReturn::from_fd)
        .unwrap_or_else(SyscallReturn::from_errno)
}
```

检查项：

- 不支持路径返回明确的 `ENOSYS`、`EOPNOTSUPP` 或领域错误；
- 权限错误、无效参数、资源耗尽、用户内存错误不要混成同一个失败；
- 新错误路径是否有测试覆盖；
- 是否避免用字符串匹配错误。

## 5.3 进程资源生命周期必须成对

新增进程、线程、FD、VM area、signal、futex 或 namespace 能力时，必须同时考虑：

- fork/clone/exec/exit/wait 的所有权转移；
- 引用计数和关闭顺序；
- 失败路径释放；
- 多线程共享和 per-thread 状态差异；
- 与文件系统、内存和调度器的交叉影响。

---

# 6. 文件系统与 I/O

## 6.1 VFS 语义与具体文件系统实现分离

VFS 层负责路径解析、fd table、权限、缓存策略和统一接口；具体文件系统负责格式、目录项、inode、block 映射和持久化语义。

禁止让 ext4/fat/ramfs/virtio-blk 细节直接污染 syscall 层。syscall 层应只依赖 VFS 或文件对象能力。

## 6.2 I/O 路径必须说明同步语义

新增 read/write/fsync/mmap/poll/async I/O 能力时，必须说明：

- 操作是同步完成、异步提交还是 lazy flush；
- 错误何时返回给调用方；
- 部分读写如何表达；
- cache 和 backing device 的一致性边界；
- 是否会阻塞调度器敏感路径；
- 对 board rootfs、测试镜像和 QEMU 设备的影响。

## 6.3 文件描述符能力要小而明确

FD 对象不应变成“什么都能做”的巨型接口。按能力拆分：

- read/write；
- seek；
- poll；
- mmap；
- ioctl；
- metadata；
- close/drop；
- path 或 dentry 相关查询。

调用方需要什么能力，就暴露什么 trait 或方法；不为了少传参数而把所有文件系统对象塞进一个全局上下文。

---

# 7. 驱动、IRQ 与 DMA

## 7.1 驱动核心与 OS glue 分离

可复用驱动 crate 应尽量独立于 ArceOS、StarryOS 或具体运行时。驱动核心关注设备协议、寄存器、队列和 DMA 描述符；OS glue 负责内存分配、睡眠/唤醒、IRQ 注册、任务调度和日志。

推荐分层：

| 层次 | 内容 |
|------|------|
| Driver Core | 寄存器、队列、协议状态机、设备命令 |
| Capability Boundary | MMIO、DMA、IRQ、clock、reset、delay、cache 维护 trait |
| Runtime Adapter | 把 OS 的 allocator、IRQ、wait queue、task API 适配成能力 |
| OS Integration | 设备注册、VFS/net/block/input 接入、配置和测试 |

## 7.2 IRQ 处理要最小化

IRQ handler 应只做必须的硬件 ack/mask、读取状态、记录事件、唤醒后半部或调度任务。复杂解析、分配、大量日志和用户态回调应放到后半部、工作队列或普通任务上下文。

检查项：

- handler 是否能在 hard IRQ 上下文运行；
- 是否持有可能睡眠的锁；
- 是否调用会分配或阻塞的路径；
- 是否正确 ack/mask/unmask；
- 是否处理共享 IRQ、spurious IRQ 和设备移除。

## 7.3 DMA 必须显式处理地址、所有权和一致性

DMA 代码必须区分：

- CPU 虚拟地址；
- CPU 物理地址；
- 设备可见 DMA 地址；
- IOMMU 映射；
- cache coherent 与 non-coherent；
- descriptor ownership 属于 CPU 还是设备。

提交 DMA 前必须保证 buffer 生命周期覆盖设备访问；设备完成前不得释放、移动或重用 buffer。non-coherent 平台必须在边界处明确 cache flush/invalidate。

---

# 8. 虚拟化

## 8.1 Host、Guest 和 Hypervisor 状态必须分离

虚拟化代码最容易把状态混在一起。每个字段、寄存器和内存对象都必须标注属于 host、guest 还是 hypervisor。

| 状态 | 示例 | 要求 |
|------|------|------|
| Host | host stack、host CPU context、host address space | guest exit 后必须恢复 |
| Guest | vCPU registers、guest page table、guest device tree | 由 VM/vCPU 生命周期拥有 |
| Hypervisor | VM config、trap handler、nested paging、emulated device | 不泄漏给 guest ABI |

不要把 host-only 状态塞进 guest state，也不要让 VM config 混入运行时可变状态。

## 8.2 二阶段地址转换要有独立边界

nested page table、EPT、Stage-2、Sv39x4/Sv48x4 等逻辑应聚合在虚拟化地址空间或架构后端中。通用 VM 逻辑只依赖“映射 guest physical memory”的能力，不直接拼具体页表格式。

检查项：

- guest physical、host physical、host virtual 是否用不同类型表达；
- 映射权限和 memory type 是否可审计；
- unmap/remap 失败是否回滚；
- TLB/VMID/ASID 刷新策略是否明确；
- 架构后端是否只暴露稳定能力。

## 8.3 设备树和虚拟设备是 guest ABI

guest FDT、ACPI table、virtio 配置、MMIO 地址、IRQ 编号和 bootargs 都会影响 guest 可见 ABI。修改这些内容必须说明兼容性影响，并用 guest 启动或 smoke 测试验证。

---

# 9. 观测与验证策略

## 9.1 观测点服务调试闭环

日志、trace、计数器和 debug dump 应服务明确问题：

- 启动卡在哪个阶段；
- IRQ 是否到达；
- 调度器是否切换；
- 页故障地址和权限是什么；
- syscall 参数和错误码是什么；
- DMA descriptor ownership 是否推进；
- guest exit reason 和 guest PC 是什么。

不要在热路径无节制打印。调试日志应可通过 feature、log level 或子系统过滤控制。

## 9.2 验证从最低风险闭环开始

推荐验证顺序：

1. 纯函数、状态机、错误转换优先写单元测试。
2. crate 或组件边界用 focused cargo/xtask 命令验证。
3. OS 行为用 QEMU test-suit 验证。
4. 架构、平台、驱动和文件系统风险用对应板卡或设备验证。
5. PR 描述记录实际运行的命令和关键通过信号。

文档-only 改动可以只做 Markdown 格式和链接检查；代码路径变化必须跑与风险匹配的构建、clippy、测试或 QEMU/board 用例。

## 9.3 回归测试应贴近失败点

修 bug 时，测试应尽量落在能稳定复现失败的最低层：

| 失败类型 | 推荐回归位置 |
|----------|--------------|
| 纯解析或转换错误 | 单元测试 |
| 状态机错误 | 模块内测试或组件测试 |
| syscall 语义错误 | Starry syscall/app 测试 |
| 调度/同步错误 | QEMU stress 或 focused kernel test |
| 驱动协议错误 | driver crate 测试、QEMU 设备测试或板卡测试 |
| boot/platform 错误 | 对应 arch/platform QEMU 或板卡启动用例 |
| 虚拟化 guest 错误 | Axvisor smoke test 或 guest 行为测试 |

不要用放宽测试、扩大 timeout、吞错误或改 success regex 掩盖真实失败。

---

# 10. 内核功能 PR 自查清单

提交内核功能 PR 前至少检查：

1. 功能放置的 crate、模块和 layer 是否正确？
2. 启动、平台、架构、OS glue 和 reusable core 的边界是否清楚？
3. 新增资源的所有权、生命周期和释放路径是否明确？
4. 锁、IRQ、原子、sleepable path 和 scheduler-sensitive path 是否区分？
5. 用户态 ABI、guest ABI、文件系统语义或硬件寄存器语义是否有兼容性说明？
6. 错误是否用 typed error 或明确 errno 表达，没有 silent fallback？
7. unsafe 是否收敛，安全前提是否写在代码旁边？
8. 驱动是否把 MMIO、DMA、IRQ、runtime adapter 和 OS integration 分层？
9. 虚拟化是否区分 host、guest 和 hypervisor 状态？
10. 验证是否覆盖主路径、失败路径和这次改动最可能破坏的边界？

## 11. 核心口号

> **内核功能先定边界，再定所有权；先写清语义，再接硬件和 ABI；先有可复现验证，再谈合入。**
