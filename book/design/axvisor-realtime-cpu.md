# Axvisor 实时 CPU 分区设计

## 1. 功能定位

Axvisor 的实时 CPU 支持采用物理 CPU 所有权分区，而不是把实时工作建模为普通 `ax-task` 高优先级线程。启用 `AX_RT_CPU=3` 并以 `SMP=4` 构建时，`pCPU0..2` 继续运行原有 ArceOS/Axvisor host runtime、VM manager、shell、普通设备和 vCPU thread，`pCPU3` 在完成最小 secondary CPU 初始化后跳入 Axvisor 的实时入口，并运行 `components/ax-rt` 提供的独立 cooperative executor。

### 1.1 设计目标

当前目标是在不破坏 Axvisor 默认行为的前提下，提供一个可观察、可扩展的单核实时执行域。这个执行域与普通 host scheduler 隔离，不进入 `ax_task::init_scheduler_secondary()`、`ax_task::run_idle()`、普通 sleep/wake、普通 IPI readiness 和普通 block runtime online 流程；host 侧只能通过显式状态快照和输出缓冲区观察 RT 核。

RT CPU 分区的第一版成功标准是“CPU 所有权不会混淆”。启用后，普通 Axvisor host 只把 host CPU 集视为可调度资源，AxVM host 只向 host CPU 集派发虚拟化初始化与 vCPU 线程；RT CPU 只运行静态内核态实时任务，不运行 guest vCPU，也不作为普通 task migration 的目标。

### 1.2 默认行为

不设置 `AX_RT_CPU` 时，构建脚本生成的 `build_info::REALTIME_CPU_ENABLED` 为 `false`，`secondary_cpu_owner()` 会把所有在线 logical CPU 都归类为 `SecondaryCpuOwner::Host`。这种情况下不会调用 `ax_realtime_secondary_main()`，不会启动 `ax-rt` executor，`INITED_CPUS` 仍等待全部 CPU，IPI readiness 和 block runtime SMP online 也保持原有路径。

默认关闭行为对回归定位很重要。RT 支持被做成 build-time opt-in 后，普通 Axvisor QEMU、板卡和 CI 流程不需要知道 RT CPU 的存在；只有显式设置 `AX_RT_CPU` 的构建才会改变 secondary CPU 所有权和 host CPU 数。

### 1.3 当前限制

当前实现只支持把最后一个 logical CPU 保留给 RT runtime，例如 4 核系统中的 `AX_RT_CPU=3`。`os/arceos/modules/axruntime/build.rs` 会拒绝 primary CPU、拒绝非最后 CPU，并把 `SMP` 解析出的 CPU 容量写入 `build_info::CPU_CAPACITY`，避免启动阶段出现 primary、设备 probe、IRQ mask 或 VM manager 所有权尚未设计清楚的中间 CPU 分区。

这些限制是实现边界，不是最终架构边界。当前 RT executor 也是 cooperative/yield 模型，`rt_sleep()` 和 `rt_delay_until()` 通过 executor loop 扫描 deadline 唤醒任务，还没有接入 RT 专属 timer IRQ；RT task 是启动时静态注册的 `fn() -> !`，不支持动态创建、跨 CPU 迁移、priority inheritance、RTOS guest 独占 CPU 或 RT-owned 设备直通。

## 2. 启动流程

实时 CPU 的启动路径从 ArceOS runtime 的 build info 开始，在 secondary CPU common init 之后分流，并在 Axvisor glue 中进入 `ax-rt`。分流点选在 `rust_main_secondary()` 内部，是因为这个位置已经完成了 CPU-local 和早期 HAL 初始化，但还没有把该 CPU 注册进普通 scheduler、IPI readiness、per-CPU IRQ 和 idle loop。

### 2.1 构建配置

`os/arceos/modules/axruntime/build.rs` 负责读取 `SMP` 和 `AX_RT_CPU`，并生成 `build_info.rs` 中的 `CPU_CAPACITY`、`REALTIME_CPU_ENABLED` 和 `REALTIME_CPU`。`cargo:rerun-if-env-changed=AX_RT_CPU` 确保切换 RT CPU 配置时重新生成运行时常量；`virtualization/axvm/build.rs` 也监听同一个环境变量，让 AxVM host CPU 计数随构建配置更新。

| 配置项 | 代码锚点 | 当前语义 |
| --- | --- | --- |
| `SMP` | `RuntimeConfig::load()` | 生成 compile-time `CPU_CAPACITY`，并作为 `AX_RT_CPU` 合法性校验依据 |
| `AX_RT_CPU` | `RuntimeConfig::load()` | 非空时启用实时 CPU 分区，并要求值等于 `SMP - 1` |
| `REALTIME_CPU_ENABLED` | `build_info_source_from()` | `axruntime::secondary_cpu_owner()` 判断是否存在 RT CPU |
| `REALTIME_CPU` | `build_info_source_from()` | 被保留给 RT runtime 的 logical CPU ID |

典型 QEMU 启动命令需要同时指定 4 核和 RT CPU。`--smp 4` 决定运行时可见 CPU 数，`AX_RT_CPU=3` 决定构建产物中的分区策略；两者必须匹配，否则构建脚本会拒绝当前不支持的布局。

```bash
AX_RT_CPU=3 cargo xtask axvisor qemu \
  --config os/axvisor/configs/board/qemu-aarch64.toml \
  --smp 4
```

### 2.2 Primary 初始化

Primary CPU 仍走 `axruntime::rust_main()` 的原有 Axvisor host 初始化路径。它初始化 allocator、paging、platform devices、scheduler、IPI、IRQ、filesystem、serial 和其他 runtime 模块，然后通过 `mp::start_secondary_cpus(cpu_id)` 启动 secondary CPU；这一步不把 primary CPU 让给 RT，因为当前实现明确禁止 `AX_RT_CPU=0`。

Primary 后续等待条件从“所有物理 CPU 都完成普通 runtime 初始化”收敛为“所有 host CPU 完成普通 runtime 初始化”。`INITED_CPUS` 的完成条件由 `is_init_ok()` 比较 `host_cpu_count()`，因此 RT CPU 进入自己的 executor 后不会让 primary 永远卡在普通 SMP init barrier 上。

### 2.3 Secondary 分流

所有 secondary CPU 首先进入 `os/arceos/modules/axruntime/src/mp.rs` 的 `rust_main_secondary(cpu_id)`。该函数先处理超过 `CPU_CAPACITY` 的 CPU park，再执行 `ax_hal::percpu::init_secondary(cpu_id)`、`ax_alloc::init_percpu_slab(cpu_id)` 和 `ax_hal::init_early_secondary(cpu_id)`；随后通过 `secondary_cpu_owner(cpu_id)` 判断 CPU 所有权。

下面的状态图描述了当前实际启动路径。RT 分流发生在普通 paging secondary、late HAL、`ax_task` secondary scheduler、IPI init 和 IRQ online 之前，因此 RT CPU 不会被注册进普通 host 调度域。

```mermaid
stateDiagram-v2
    [*] --> SecondaryEntry
    SecondaryEntry --> Parked: cpu_id >= CPU_CAPACITY
    SecondaryEntry --> MinimalCpuInit: cpu_id < CPU_CAPACITY
    MinimalCpuInit --> RtEntry: SecondaryCpuOwner::Realtime
    MinimalCpuInit --> HostSecondary: SecondaryCpuOwner::Host
    RtEntry --> RtExecutor: ax_realtime_secondary_main
    HostSecondary --> HostScheduler: init_scheduler_secondary
    HostScheduler --> HostReady: INITED_CPUS += 1
    HostReady --> HostIdle: ax_task::run_idle
    Parked --> Parked: wait_for_irqs
```

`SecondaryCpuOwner::Offline` 当前只用于容量外 CPU park，不是面向用户的配置能力。后续如果需要显式 offline CPU，应补齐启动日志、IRQ mask、VM placement 和 host parallelism 语义，而不是直接把裸 CPU ID 放进多个模块分别判断。

### 2.4 RT 入口

RT CPU 的 Axvisor 入口是 `os/axvisor/src/realtime.rs` 中的 `#[unsafe(no_mangle)] pub extern "Rust" fn ax_realtime_secondary_main(cpu_id: usize) -> !`。`axruntime::run_realtime_secondary(cpu_id)` 通过外部符号调用这个入口，使通用 runtime 只负责 CPU 分流，Axvisor glue 负责选择 RT task、时间源和状态发布。

入口函数记录初始 heartbeat/watchdog 时间戳，打印 `Realtime CPU {cpu_id} entered Axvisor RT entry; running isolated executor.`，然后调用 `ax_rt::run_realtime_cpu(cpu_id, &RT_TASKS, monotonic_time_nanos)`。从这一点开始，RT CPU 不返回 `axruntime`，也不会继续执行普通 secondary 的 paging、late HAL、scheduler、IPI 或 IRQ online 代码。

## 3. 侵入点

实时 CPU 支持尽量把侵入限制在 CPU 所有权、启动同步、IRQ/IPI 范围、AxVM host CPU 计数和 Axvisor shell glue。核心原则是：通用 ArceOS runtime 只知道某个 secondary CPU 是否属于 host，`ax-rt` 不依赖 Axvisor，Axvisor 只作为第一个集成者提供 RT 入口和 demo tasks。

### 3.1 ArceOS Runtime

`axruntime` 的修改集中在 build info、secondary ownership、host CPU count 和普通 SMP barrier 上。`secondary_cpu_owner(cpu_id)` 读取 `build_info::REALTIME_CPU_ENABLED` 与 `build_info::REALTIME_CPU`，将保留 CPU 归类为 `Realtime`；`host_cpu_count()` 遍历 `ax_hal::cpu_num()` 并只统计 `Host` CPU。

| 位置 | 代码锚点 | 侵入原因 |
| --- | --- | --- |
| `axruntime/build.rs` | `RuntimeConfig::load()` | 把 `AX_RT_CPU` 固化为构建常量，并校验当前只支持最后一个 CPU |
| `axruntime/src/lib.rs` | `SecondaryCpuOwner` | 给 secondary CPU 分流提供单一所有权事实 |
| `axruntime/src/lib.rs` | `host_cpu_count()` | 让 `INITED_CPUS` 等待 host CPU，而不是等待 RT CPU |
| `axruntime/src/lib.rs` | `run_realtime_secondary()` | 通过外部符号跳入 Axvisor RT 入口 |
| `axruntime/src/mp.rs` | `rust_main_secondary()` | 在普通 scheduler 初始化前把 RT CPU 分出去 |

这些点是必须侵入 ArceOS runtime 的部分，因为只有 runtime 能在 secondary CPU 进入普通调度器前做出所有权决策。如果把判断放在 Axvisor main 或 shell 层，RT CPU 已经被普通 scheduler、IPI 和 IRQ 框架注册，隔离就无法成立。

### 3.2 IRQ 与 IPI 范围

普通 timer IRQ 和 IPI handler 的 per-CPU 注册范围改为 host CPU 集。`init_percpu_irq()` 在 BSP 上使用 `ax_hal::irq::CpuMask::first_n(host_cpu_count())` 请求 timer IRQ 和 IPI IRQ，避免把保留的 RT CPU 纳入普通 scheduler tick 或普通 IPI handler。

当 host CPU 数少于 `ax_hal::cpu_num()` 时，primary 会跳过 `ax_ipi::wait_for_all_cpus_ready()` 和 `fs::online_smp()`，并打印 `Skip block runtime SMP online while Axvisor realtime CPU split is active.`。这是保守处理：当前 block runtime 的 SMP online 语义仍默认所有 CPU 都属于普通 host，如果把 RT CPU 包进去会重新引入等待或 affinity 风险。

### 3.3 AxVM Host

AxVM 通过 `virtualization/axvm/src/host/arceos.rs` 的 `ArceOsHost` 适配 ArceOS runtime。`HostCpu::cpu_count()` 返回 `host_cpu_count()`，该函数在 `AX_RT_CPU` 等于最后一个 CPU 时返回 `cpu_num - 1`；因此 AxVM host virtualization enable、vCPU placement 和相关 host-side CPU 枚举只看到 host CPU 集。

这个修改解决了早期原型中 host 卡在 `Enabling hardware virtualization support on all cores...` 的问题。卡住原因是 AxVM 仍等待 core3 执行普通 host virtualization 初始化，而 core3 已经进入 RT executor；把 AxVM host CPU count 收敛到 host CPU 集后，core0..2 完成初始化即可继续进入 Axvisor shell 和 VM 管理路径。

### 3.4 Axvisor Glue

Axvisor 的侵入点在 `os/axvisor/src/realtime.rs` 和 shell 命令。`realtime.rs` 提供 `ax_realtime_secondary_main()`、Axvisor demo task 表、heartbeat/watchdog 统计、RT 输出读取 re-export，以及 `CpuOwner`/`cpu_owner()` 等 Axvisor 侧查询函数；shell 侧的 `os/axvisor/src/shell/command/rt.rs` 提供 `rt status`、`rt console` 和 `rt shell`。

Axvisor glue 不实现通用 scheduler core。通用 RT executor 已经抽到 `components/ax-rt`，所以 Axvisor 只传入静态任务数组和 `monotonic_time_nanos` 时间源；后续其他内核或子系统如果需要同类单核 cooperative RT executor，可以依赖 `ax-rt` 而不依赖 Axvisor。

## 4. 实时执行域

`components/ax-rt` 是与 `ax-task` 平级的 `#![no_std]` crate，承载 RT task 描述、上下文切换、cooperative yield/sleep、状态快照、RT mutex 和固定容量输出缓冲区。它有意不依赖 `axvisor`、`axvm` 或 `ax-std`，只要求集成者在入口处提供静态任务表和单调时间源。

### 4.1 任务模型

RT task 使用 `RtTask::new(name, period_nanos, run)` 静态声明，`run` 是不会返回的 `fn() -> !`。`ax-rt` 当前最多支持 `MAX_RT_TASKS = 8` 个任务；`run_realtime_cpu()` 会记录 CPU ID、entry timestamp、任务表指针和时间源，然后进入 executor loop。

| API 或类型 | 代码锚点 | 当前功能 |
| --- | --- | --- |
| `RtTask` | `components/ax-rt/src/task.rs` | 静态任务描述，包含名称、周期和入口函数 |
| `run_realtime_cpu()` | `components/ax-rt/src/executor.rs` | 初始化全局 RT 状态并在当前 CPU 上运行 executor |
| `rt_yield_now()` | `components/ax-rt/src/executor.rs` | 把当前 task 标记为 `Ready` 并切回 executor |
| `rt_delay_until()` | `components/ax-rt/src/executor.rs` | 把当前 task 标记为 `Delayed`，直到指定 deadline |
| `rt_sleep()` | `components/ax-rt/src/executor.rs` | 基于当前时间计算 deadline 并 delay |
| `rt_exit_current_task()` | `components/ax-rt/src/executor.rs` | 把当前 task 标记为 `Exited`，不再调度 |

executor loop 以 round-robin 方式扫描任务，先调用 `wake_expired_tasks(now)` 把到期的 `Delayed` task 改回 `Ready`，再把 ready task 切到独立上下文运行。当前没有抢占，任务必须主动调用 RT API 切回 executor；因此任务函数不能执行不可控长时间 busy loop，也不能调用普通 host sleepable API。

### 4.2 上下文隔离

`components/ax-rt/src/context.rs` 为 executor 和每个 RT task 准备独立 stack、`axcpu::TaskContext` 和 RT 私有 current-thread header，并通过 CPU-local current-thread switch transaction 完成上下文切换。这里复用的是底层 CPU context 和 CPU-local 机制，而不是复用 `ax-task` 的 run queue、`CurrentTask`、wait queue 或 timer wheel。

这种分层是隔离性的关键。RT CPU 上不存在普通 task migration，RT task 的 `runs` 计数表示任务主动 yield、delay、block 或 exit 的次数；host 只能通过 `status()` 的原子快照观察状态，不能直接把普通 `AxTaskRef` 放进 RT executor。

### 4.3 同步原语

`components/ax-rt/src/sync.rs` 提供两种面向 RT task 的 cooperative 同步原语，二者共享同一套 `waiters: AtomicUsize` bitmask 阻塞/唤醒实现：阻塞时把当前 RT task 标记为 `Blocked` 并 yield 回 executor，唤醒时按 effective priority 选出优先级最高的 waiter 改回 `Ready`。

`RtMutex` 是带所有权的 cooperative sleepable mutex，内部用 `owner: AtomicUsize` 记录持有者。它支持 recursive locking（`recursion_depth` 计数，同一 task 重入加锁、逐层 unlock 释放）与 priority inheritance（waiter 阻塞时把自身 effective priority 捐给 owner，owner 完全释放时恢复 base priority），用来约束 RT task 之间的优先级反转。

`RtSemaphore` 是 counting semaphore，用 `permits: AtomicUsize` 记录可用许可。`acquire()` 取一个许可，无许可时阻塞当前 task；`try_acquire()` 非阻塞尝试；`release()` 归还一个许可并唤醒优先级最高的 waiter。与 mutex 不同，semaphore 没有单一 owner：任何 task 都可以 `release`，因此它**不做 priority inheritance**，定位是信号量式的“唤醒”和有界资源计数（producer/consumer、资源池），而不是互斥。

这两种原语都只适合单核 cooperative RT 域内部使用。它们都**不是 IRQ-safe**，不支持 IRQ context lock/unlock、跨 CPU owner 或与普通 host mutex 混用；后续接入真实 RT IRQ 或设备 hot path（尤其是从 ISR 里 `release` 一个 semaphore 去唤醒 RT worker task）时，需要重新设计 IRQ-safe 的阻塞/唤醒路径和优先级语义。

### 4.4 输出通道

RT task 不直接写 host UART，也不拿普通 console lock。`components/ax-rt/src/output.rs` 提供固定容量 1024 字节 ring buffer，RT 侧用 `rt_output_write()` 和 `rt_output_write_decimal()` 写入，host shell 用 `rt_read_output()` 拉取。

这个输出通道是临时但安全的观测面。`rt console` 和 `rt shell` 当前只是 drain RT output buffer，并不改变 Axvisor guest console mux 的 attached VM 模型；如果未来需要交互式 RT shell，应把 console attachment 显式扩展为 Host shell、Guest console 和 Realtime console 三态。

## 5. 已实现功能

当前实现已经完成从构建配置到 RT CPU 启动、独立 executor、状态观测和 Linux guest 共存的最小闭环。它还没有实现真实 RT timer IRQ、mailbox、RT-owned device 或 deadline scheduler，所以文档和运行日志都应把它称为 isolated cooperative realtime executor。

### 5.1 Axvisor Demo Tasks

`os/axvisor/src/realtime.rs` 注册了三个静态 demo task，用来证明 RT CPU 能长期独立运行并提供可观察状态。`heartbeat` 以 1ms 周期更新 heartbeat 计数和时间戳，`watchdog` 以 100ms 周期更新时间戳，`hello` 每 1s 向 RT output buffer 写一次消息，输出 5 次后调用 `rt_exit_current_task()` 进入 `Exited` 状态。

| 任务名 | 周期 | 代码锚点 | 行为 |
| --- | --- | --- | --- |
| `heartbeat` | 1,000,000 ns | `heartbeat_task()` | 更新 `RT_HEARTBEATS` 和 `RT_LAST_HEARTBEAT_NANOS` |
| `watchdog` | 100,000,000 ns | `watchdog_task()` | 更新 `RT_WATCHDOG_RUNS` 和 `RT_LAST_WATCHDOG_NANOS` |
| `hello` | 1,000,000,000 ns | `hello_task()` | 输出 `hello from RT task N/5`，第 5 次后退出 |

`heartbeat` 和 `watchdog` 共享 `RT_SAMPLE_MUTEX`，用于覆盖 `RtMutex` 的基本 lock/yield/wake 路径。这个 demo mutex 不是跨 CPU 通信手段，只验证 RT executor 内部 cooperative blocking 状态能被调度器重新唤醒。

### 5.2 Shell 观测

Axvisor shell 注册了 `rt` 命令树。`rt status` 调用 `ax_rt::status()` 并打印 RT CPU、运行状态、task context 数、heartbeat、executor iterations、entry timestamp、last heartbeat/watchdog timestamp 和每个 task 的表格；`rt console` 与 `rt shell` 轮询 `rt_read_output()`，把 RT output buffer 中的文本打印到 host shell。

`rt status` 在未设置 `AX_RT_CPU` 或 RT 入口尚未执行时显示 `RT CPU: none` 和 `State: offline`。这不是错误，而是默认关闭或尚未进入 RT executor 的可诊断状态；只有启用并成功分流后，状态才会变成 `running` 并出现任务统计递增。

### 5.3 Guest 共存

已验证在 `AX_RT_CPU=3 --smp 4` 下，Linux guest 可以在 host CPU 集上启动，RT core3 不被 vCPU 占用。关键条件是 VM 配置必须存在；如果不传 `--vmconfigs` 且 rootfs 中 `/guest/vm_default` 为空，`vm list` 输出 `No virtual machines found.` 是 Axvisor 当前配置加载行为，不是 RT CPU 分区导致的失败。

临时 memory-mode Linux VM config 可用于验证 host/RT 共存。host 侧只看到 core0..2，RT 侧持续执行 executor；日志应包含 host virtualization 初始化完成、Axvisor shell 出现，以及 RT CPU 进入 isolated executor 的提示。

## 6. 验证与后续

验证应覆盖默认关闭和显式启用两条路径。默认关闭用于证明改动没有回归普通 Axvisor；显式启用用于证明 core3 没有进入普通 scheduler，并且 host CPU 集上的 Axvisor shell、VM 管理和 guest 启动仍可用。

### 6.1 本地验证

已完成的基础验证包括 `cargo fmt`、`cargo xtask clippy --package ax-rt`、4 核 Axvisor build，以及 `AX_RT_CPU=3` 的 QEMU smoke。Axvisor crate 的显式 clippy 曾被既有无关 warning 阻塞，位置是 `platforms/somehal/src/arch/aarch64/gic/v3.rs` 的 `clippy::unnecessary_cast`。

推荐的快速验证命令是先构建，再运行 QEMU smoke。只改文档时不需要重新跑这些命令；改 `axruntime`、`ax-rt`、AxVM host adapter 或 Axvisor RT glue 时应至少跑 `cargo fmt`、目标 crate clippy 和一次 RT QEMU 启动。

```bash
cargo fmt
cargo xtask clippy --package ax-rt
AX_RT_CPU=3 cargo xtask axvisor build \
  --config os/axvisor/configs/board/qemu-aarch64.toml \
  --smp 4
AX_RT_CPU=3 cargo xtask axvisor qemu \
  --config os/axvisor/configs/board/qemu-aarch64.toml \
  --smp 4
```

### 6.2 后续工作

后续扩展应继续保持 CPU 所有权事实单一，避免在 VM、IRQ、scheduler 或 shell 中复制“最后一个 CPU 是 RT CPU”的判断。优先事项是把 busy polling sleep 改成 RT 专属 timer IRQ，把 host/RT 通信用 bounded mailbox 表达，并在接入任何 RT-owned IRQ 或设备前补齐设备所有权配置和 probe 排除规则。

未来如果要支持 RTOS guest 独占物理 CPU，应作为新的 dedicated pCPU guest 设计处理。那条路径涉及 guest vCPU 与物理 CPU 绑定、虚拟中断、timer、设备直通和 teardown，不能直接复用当前 host 内核态 `ax-rt` executor 的任务模型。
