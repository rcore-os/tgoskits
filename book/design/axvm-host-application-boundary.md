# AxVM Host/Application 边界

## 状态

本文档记录 Axvisor 与 AxVM 之间的依赖边界。本次调整保持行为不变：现有宿主操作被收口到由所有者提供的窄接口之后，VM 配置和设备交接语义不变。

## 问题

Axvisor 是选择 VM 策略并编排 VM 生命周期的应用。此前它直接依赖 `ax-hal` 执行控制台和 CPU 操作，并依赖 `ax-driver` 完成 x86 QEMU 块设备的 INTx 交接。这些依赖把 AxVM ArceOS 宿主适配器的实现类型暴露给应用，也允许 VM 生命周期绕过其所有者。

Axvisor 此前还导出了板级驱动 feature 的别名。这既重复了 `ax-driver` 的硬件能力名称，也使这些 feature 究竟属于应用还是构建配置变得不明确。

## 目标

- Axvisor 通过 `axvm` 使用 VM 及 VM 相关的宿主操作。
- 不在 Axvisor API 中暴露宿主 HAL 和驱动类型。
- 保持宿主控制台只有一个读取者的约束。
- 保持 x86 直通流程的操作顺序和错误行为。
- 在构建配置中直接选择板级驱动 feature，不在 Axvisor 中暴露驱动 API 或重复的 feature 别名。

## 非目标

- 使 AxVM 脱离 ArceOS 宿主适配器。
- 定义通用的公共 HAL 或驱动抽象。
- 把宿主控制台轮询改为中断驱动。
- 把 QEMU 块设备直通配置泛化到任意 PCI 设备。
- 修改 guest 配置语法、IRQ 路由策略或 VM 启动顺序。

## 边界方案

没有保留对 HAL 和驱动 API 的源码级直接访问，因为这会继续让多方共同持有宿主状态。没有导出完整的内部 `HostCpu`、`HostMemory`、`HostTime` 和 `HostPlatform` trait，因为这会把面向实现的运行时能力扩大为宽泛的公共 API。也没有在 Axvisor 中增加原始 HAL 和驱动包装，因为仅重命名依赖不能修正所有权。

选定的边界只暴露 Axvisor 应用必须编排的操作：

- `axvm::host::console` 控制并访问物理宿主控制台。
- `axvm::host::cpu` 报告控制台读取任务选核所需的宿主 CPU 拓扑。
- `axvm::host::x86` 持有固定的 QEMU 块设备 IRQ 路由和交接流程。

公共函数只使用 AxVM 类型或普通数据。`ax-hal` IRQ 类型、`ax-driver` PCI 描述符和内部宿主 trait 均保持私有。

## 所有权与生命周期

Axvisor 仍是策略所有者：它决定使用文件系统后端的直通 VM 何时需要释放宿主资源，以及 guest 何时可以继续启动。AxVM 持有具体机制并保持以下顺序：

1. VM 注册后，AxVM 解析宿主 PCI INTx 绑定，并安装 guest IOAPIC 转发路由和激活回调。
2. Axvisor 通过 AxVM 请求关闭宿主文件系统。
3. AxVM 请求宿主驱动为 QEMU 块设备直通做好准备。
4. guest 启动过程中转发路由激活时，AxVM 解除宿主 INTx 源的屏蔽。

路由发现和设备准备仍采用尽力而为的行为；不支持的宿主配置会被记录，这与此前行为一致。安装 AxVM 转发路由失败仍是 VM 初始化错误。

控制台 API 只能在任务上下文使用。Axvisor 的多路复用器仍是唯一的物理输入读取者：它关闭输入中断并逐字节轮询。该边界不引入额外缓冲区、读取者或 IRQ 回调。

## Feature 所有权

Axvisor 的板级和测试配置直接选择嵌套的 `ax-driver/<feature>` feature。Axvisor 的可选依赖只是 Cargo feature 路由锚点：仅在配置选择某个驱动 feature 时启用，而 Axvisor 源码从不调用驱动 API。AxVM 则为执行 QEMU 块设备交接的 `host-fs` 能力单独启用其 x86 PCI 驱动依赖。

## Rust 标准库边界

AxVM 是仅支持 Rust `std` 的 crate。生产环境中的 Axvisor 消费者使用仓库为所请求架构提供的 RustStd/musl target 构建；axbuild 把 bare-metal 请求 target 映射到对应的 `*-unknown-linux-musl` PIE target，并同时构建 `std` 与 `panic_abort`。不再支持把 AxVM 独立构建为仅含 `core`/`alloc` 的 bare-metal 库。

因此，AxVM 的集合、所有权内存、格式化、原子操作、时间、线程、`OnceLock` 和任务上下文 `Mutex` 状态都使用真实 `std` 接口，不使用 `ax-std` 中的同名 mini-std 替代品。宿主测试中的任务上下文 mutex 中毒后会保留并恢复受保护状态；生产构建采用 panic abort，不会在 unwind 后观察到中毒状态。

`ax-std` 只保留为 AxVM 的 ArceOS 扩展边界。HAL、任务、中断、抢占 guard、per-CPU 和非休眠锁能力只能通过 `ax_std::os::arceos` 访问。特殊锁的语义化名称用于说明调用上下文约束：

- `IrqSafeMutex` 在持锁期间关闭抢占和本地中断。
- `NoPreemptMutex` 关闭抢占，且不得与 IRQ handler 共享。
- `RawSpinLock` 不改变任何执行状态；调用方必须已经排除 CPU 迁移和中断重入。

任务上下文注册表、CPU_ON acknowledgement、deferred worker 所有权、固件暂存和生命周期错误使用 `std::sync::Mutex`。VM/runtime 快照、待处理中断队列、vCPU 内部状态、timer wheel 和架构中断控制器状态继续使用非休眠锁，因为其调用链可能进入 IRQ、IPI、禁抢占或 guest-entry 路径。这些路径禁止获取可能阻塞的标准 mutex。混合对象把两类所有权域拆为独立字段；回调、通知和 IPI 均在锁临界区之外执行。

## 验证

Cargo metadata 契约测试禁止 AxVM 直接依赖 `ax-hal`、`ax-kernel-guard`、`ax-percpu`、`ax-lazyinit`、`ax-kspin`、`ax-sync` 或 `spin`；源码检查同时禁止这些实现路径和遗留的 `alloc` 访问。axbuild 测试要求 x86_64、AArch64、RISC-V 和 LoongArch64 的 Axvisor 请求全部选择对应 RustStd/musl target，并使用 `std + panic_abort`。针对性的 clippy、AxVM 宿主测试、四架构 Axvisor 构建和可用的 QEMU smoke 测试共同验证运行时边界保持不变。
