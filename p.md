# 删除 `ax-feat` 并重建 ArceOS Feature 归属

## 目标

彻底删除 `os/arceos/api/axfeat`，重新建立 ArceOS feature 的职责层级，使 feature 关系直接、简单、可审计。

本方案不保留旧兼容：

- 删除 `ax-feat` crate。
- 删除所有 `ax-feat/*` feature 前缀。
- 删除 `axfeat` 历史别名。
- 不提供废弃兼容 crate。
- 不引入新的 feature 聚合 crate 替代 `ax-feat`。

最终状态应满足：

- 每个 feature 只由最贴近真实职责的 crate 拥有。
- API crate 只控制 API 是否暴露。
- Runtime crate 只控制系统装配和启动初始化。
- User library crate 只控制用户侧门面能力。
- StarryOS、Axvisor 等上层系统直接组合所需底层能力，不再通过 ArceOS API 聚合层转发。

## 当前问题

`ax-feat` 当前只有 feature 转发，没有实质 API。它把以下职责混在一起：

- 用户可见 feature 名表。
- runtime 初始化装配。
- HAL、task、sync、driver、fs、net、display、input 等模块 feature 转发。
- StarryOS / LKM / axbuild 的跨系统 feature 前缀入口。

这会带来几个问题：

- feature 的真实所有权不清楚。
- `ax-std`、`ax-api`、`ax-posix-api` 与 `ax-feat` 重复传递同一批底层 feature。
- `ax-runtime` 已经有装配逻辑，但 `ax-feat` 又在外层重复描述依赖。
- 上层系统依赖 `ax-feat` 后，很难判断它实际打开了哪些模块和隐式前置条件。
- 新能力接入时容易形成“先加到底层，再加到 runtime，再加到 ax-feat，再加到 ax-std”的机械链条。

## Feature 保留标准

清理 `ax-feat` 时不能只做机械替换，还要重新判断每个 feature 是否有必要存在。

一个 feature 只有满足以下任一条件时才应保留：

- 能显著裁剪镜像体积、依赖集合或初始化路径，例如 `fs`、`net`、`display`、`input`、`multitask`。
- 选择真实不同的实现、格式、算法或硬件路径，例如 `ax-fs-ng/ext4`、`ax-fs-ng/fat`、`ax-task/rr`、`ax-task/cfs`。
- 控制上层公开 API 是否存在，例如 `ax-api/fs`、`ax-posix-api/net`、`ax-std/fs`。
- 控制平台或 CPU 能力，例如 `smp`、`irq`、`fp-simd`、`hv`、`uspace`、`xuantie-c9xx`。
- 控制可选诊断、调试或加固能力，例如 `lockdep`、`backtrace`、`dwarf`、`stack-guard-page`、`stack-protector`。

以下情况不应继续作为 feature：

- 空 feature，或没有任何行为差异的 marker，例如当前 `ax-fs-ng/std`。
- 只为了转发另一个 crate 的 feature 而存在的 feature，例如 `ax-feat/*`。
- crate 正常工作所必需的能力。必选能力应直接成为默认实现，而不是继续暴露给用户选择。
- 某个上层能力已经必然包含的内部实现细节。例如 FIFO 是 `multitask` 的默认调度器，task stack canary 是 multitask task 栈基础防护时，不应再提供单独 feature。

## Feature 命名规则

清理后的 feature 名应短、稳定，并且能直接表达真实能力。命名优先级如下：

- 已形成行业或架构共识的缩写直接保留，例如 `smp`、`irq`、`ipi`、`tls`、`rtc`、`dma`、`hv`。
- 文件系统格式、调度算法、协议和设备型号直接使用真实名，例如 `ext4`、`fat`、`rr`、`cfs`、`virtio-net`、`aic8800`。
- 避免为了说明所属层级而加前缀，例如不用 `fs-ext4` / `fs-fat`，因为 feature 已经在具体 crate 下有命名空间，且依赖关系会表达它属于 `fs`。
- 避免过长的多段连字符名；只有当它是成熟术语、协议名或驱动名时才保留，例如 `stack-protector`、`virtio-net`。
- 不为“看起来统一”而发明难懂缩写；短名必须仍然能让使用者判断真实含义。
- Cargo feature 名是 per-crate 的；不同 crate 下的同名 feature 在 Cargo 层面互不相同。本方案允许 `ax-api/fs`、`ax-runtime/fs`、`ax-std/fs` 这类同名 feature 表达同一能力的不同职责切面，但调用关系必须通过各 crate 的 `Cargo.toml` 显式映射审计，不能跨 crate 推断等价性。

## 优化后各层 Feature 清单

### 应删除或默认化的 Feature

| Feature | 处理方式 | 原因 |
| --- | --- | --- |
| `ax-feat/*` | 全部删除 | 纯聚合转发，职责重复 |
| `axfeat/*` | 全部删除 | 历史别名，不保留兼容 |
| `ax-fs-ng/std` | 删除 | 当前为空 feature，没有行为差异 |
| `ax-fs-ng/times` | 删除 feature，行为默认启用 | 文件时间戳字段和 POSIX stat 表面是基础文件系统语义；无 wall time provider 时已有 0 时间回退，不应作为编译期开关 |
| `ax-runtime/times` / `ax-std/times` / `ax-libc/times` | 删除 | 时间戳支持不再作为上层可选能力暴露 |
| `ax-runtime/dma` | 删除 | 当前只是 `paging` 别名，runtime 没有独立 DMA 初始化路径 |
| `ax-task/stack-canary` | 删除 feature，行为并入 `multitask` 默认实现 | 当前 `ax-task/multitask` 已强制启用它，不存在真实“不启用 task 栈 canary 的 multitask”配置 |
| `ax-task/fifo` / `ax-std/fifo` / 旧 `sched-fifo` | 删除 feature，FIFO 作为默认调度器 | `ax-task` 在未选择 `rr` / `cfs` 时天然使用 FIFO；单独 feature 只表达默认值，没有裁剪或选择意义 |
| `sched-rr` / `sched-cfs` 旧命名 | 删除旧名 | 改为 `rr` / `cfs`，避免在 `ax-task` 命名空间内重复 `sched-` 前缀 |
| `ax-std/dynld` | 不新增 | `ext-ld` 的真实含义是 external linker，不是 dynamic loader；不引入误导性新名字 |
| `aic8800-wifi` 旧命名 | 删除旧名 | 改为 `aic8800`；设备型号已足够表达真实驱动目标 |
| `dummy-if-not-enabled` 旧命名 | 删除旧名 | 改为 `stubs`，直接表达生成 stub API 的行为 |
| `ext4fs` / `fatfs` 旧命名 | 删除 | 改为统一的 `ext4` / `fat` 命名 |

### 保留 Feature 必要性复核

下表按唯一能力名复核。若同一名字出现在 `ax-runtime`、`ax-std`、`ax-libc` 或 API 门面中，上层 feature 只作为直接映射入口保留，不代表底层能力被重复定义。

| Feature / 能力 | 复核结论 |
| --- | --- |
| `smp` | 保留。单核 / 多核启动、per-CPU、调度迁移、C ABI 中 pthread mutex layout 都有真实差异。 |
| `irq` | 保留。中断初始化、timer、IRQ wait/waker、设备 IRQ 注册均可裁剪。 |
| `ipi` | 保留。完整 IPI 队列 / 回调 / TLB shootdown 路径不是 `irq` 的必选子集。 |
| `wake-ipi` | 保留。它是只确认 SGI、唤醒 idle CPU 的轻量路径，可在没有完整 `ax-ipi` 队列时使用。 |
| `fp-simd`、`xuantie-c9xx`、`hv` | 保留。分别对应 CPU 扩展状态、具体 CPU errata / 扩展、虚拟化能力。 |
| `rtc` | 保留。不是所有平台都有 RTC，且会改变设备 probe 与 boot 时间输出。 |
| `paging` | 保留。无页表 / 有页表 runtime 差异巨大，且影响 DMA、framebuffer、地址空间、page fault。 |
| `tls` | 保留。TLS 初始化和 task TLS 字段不是所有配置必需。 |
| `uspace` | 保留。用户地址空间、页表切换 API、HAL / task wiring 都是可选能力。 |
| `multitask` | 保留。单任务和 scheduler runtime 是根本不同的执行模型。 |
| `preempt` | 保留为 `ax-task` 局部能力。抢占计数、抢占调度和 kernel guard 行为可独立于基础 multitask 审计。 |
| `rr`、`cfs` | 保留。它们是真实调度算法选择；FIFO 是默认值，不再作为 feature。 |
| `stack-guard-page` | 保留。guard page 依赖 paging / TLB flush / 诊断路径，成本和语义都不是必选。 |
| `stack-protector` | 保留。它控制 compiler-inserted canary runtime 符号，不是普通 task 栈 canary。 |
| `lockdep` | 保留。诊断能力，会引入锁依赖跟踪状态和跨 crate wiring。 |
| `task-ext`、`tracepoint-hooks` | 保留。它们分别暴露外部 task 扩展接口和 tracepoint hook，不是基础 task 必选行为。 |
| `alloc` | 保留。`ax-api` / `ax-posix-api` / `ax-std` 仍支持无 alloc 的更小 API 表面。 |
| `global-allocator` | 保留为 `ax-alloc` 局部 / `std-compat` 支撑能力；它不是普通应用 feature。 |
| `tlsf`、`buddy-slab` | 保留。它们是真实 allocator 后端选择。 |
| `tracking` | 保留。内存分配跟踪是可选诊断能力。 |
| `fs` | 保留。文件系统 API、block runtime、rootfs 初始化和 fd 集成可整体裁剪。 |
| `vfs` | 保留。它控制 `ax-fs-ng` 高层 VFS / cache / open-options / context API 是否编译。 |
| `ext4`、`fat` | 保留。当前仓库确有两种 rootfs / 磁盘格式实现。 |
| `net` | 保留。网络设备、网络栈和 socket API 可整体裁剪。 |
| `vsock` | 保留。vsock 是网络栈上的可选协议 / 设备路径。 |
| `dns` | 保留为 `ax-std` 网络行为开关。它只控制 `ToSocketAddrs` 是否进行 DNS 查询，并应依赖 `net`。 |
| `display`、`input` | 保留。图形和输入子系统是可选设备能力。 |
| `usb`、`virtio-*`、`aic8800` | 保留。它们是真实设备 / 总线 / 驱动选择。 |
| `dma` | 保留在 API 门面。它暴露显式 DMA 分配 API；runtime 不保留 `dma`，因为 runtime 没有独立 DMA 装配代码，DMA 在 runtime 侧只需要 `paging` 支撑。 |
| `axklib` glue | 不作为用户 feature 暴露。`ax-runtime` 默认提供 `axklib::Klib` glue；没有 `paging` / `irq` 时相关内存映射和 IRQ 操作返回 `Unsupported`。动态平台基础路径会使用 `axklib::mmio`，因此该 glue 不能挂在 `paging`、`fs` 或 `net` 等可选 feature 后面。 |
| `fd`、`pipe`、`select`、`poll`、`epoll` | 保留。POSIX fd 表和 I/O multiplexing API 可分别裁剪，`fs` / `net` 通过 `fd` 组合。 |
| `stubs` | 保留在 `ax-api`。它用于生成未启用 API 的 stub 符号，不是 runtime 能力。 |
| `std-compat` | 保留。它改变 lang items / libc compat / global allocator 入口，属于真实构建模式。 |
| `ext-ld` | 保留。它控制 `ax-runtime/build.rs` 生成可扩展链接脚本 `runtime.x` 而非最终 `linker.x`，Axvisor / StarryOS / LKM 实际依赖该模式追加自定义 section。 |
| `backtrace`、`dwarf` | 保留。基础栈回溯和 DWARF 符号化依赖集合明显不同。 |
| `copy` | 保留为 `ax-mm` 局部能力。它只在需要复制页表映射时启用 `ax-page-table-multiarch/copy-from`。 |

### ArceOS 底层模块层

| Crate | 保留 feature | 默认化 / 删除 |
| --- | --- | --- |
| `ax-hal` | `smp`, `irq`, `ipi`, `fp-simd`, `xuantie-c9xx`, `rtc`, `paging`, `tls`, `uspace`, `hv` | `host-test`、`axvisor-linker` 只作为测试 / 构建内部 feature，不进入用户能力层 |
| `ax-task` | `multitask`, `irq`, `ipi`, `preempt`, `smp`, `stack-guard-page`, `lockdep`, `task-ext`, `tls`, `tracepoint-hooks`, `uspace`, `rr`, `cfs` | 删除 `stack-canary` 独立 feature；删除 `sched-fifo`，FIFO 为默认调度器；`host-test` / `test` 只作为测试内部 feature |
| `ax-sync` | `multitask`, `lockdep` | 无 |
| `ax-alloc` | `global-allocator`, `tlsf`, `buddy-slab`, `tracking` | 无 |
| `ax-mm` | `copy` | 无；仅当调用方需要复制页表映射时启用 |
| `ax-fs-ng` | `vfs`, `ext4`, `fat`, `lockdep` | 删除 `std` 和 `times`；时间戳行为默认编译 |
| `ax-net` | `vsock` 等真实网络栈变体 | 无 |
| `ax-display` | 无 feature，作为 display 能力模块被上层可选依赖 | 无 |
| `ax-input` | 无 feature，作为 input 能力模块被上层可选依赖 | 无 |
| `axbacktrace` | `alloc`, `dwarf` | 无 |

### `ax-runtime`

`ax-runtime` 只保留会改变 runtime 初始化或全局系统装配的 feature。

| 保留 feature | 含义 |
| --- | --- |
| `irq` | 中断初始化、IRQ 分发、驱动 IRQ 集成 |
| `ipi` | 完整 IPI 队列 / 回调支持；清理时必须补齐 `irq`、`dep:ax-ipi`、`ax-hal/ipi`、`ax-task?/ipi` wiring |
| `wake-ipi` | 轻量 idle wake IPI handler |
| `smp` | 多核启动和 secondary CPU runtime 流程 |
| `uspace` | user address space 相关 HAL / task wiring，包括任务切换页表 API |
| `paging` | 页表、地址空间和 page fault runtime wiring |
| `multitask` | task scheduler runtime 初始化 |
| `tls` | runtime TLS 设置 |
| `fs` | 文件系统 runtime 基础栈：block runtime、rootfs 初始化、`ax-fs-ng/vfs`、page cache、block IRQ / task ops |
| `ext4` | `fs` + `ax-fs-ng/ext4` |
| `fat` | `fs` + `ax-fs-ng/fat` |
| `net` | `irq` + `multitask` + `paging` + 网络设备和网络栈注册 |
| `vsock` | `net` + vsock 设备 / 协议支持 |
| `display` | `paging` + display 设备注册 |
| `input` | `paging` + input 设备注册 |
| `rtc` | RTC 设备支持 |
| `aic8800` | SG2002 AIC8800 Wi-Fi runtime bring-up |
| `std-compat` | Rust std 兼容 runtime hook |
| `ext-ld` | 生成可扩展链接脚本 `runtime.x` 而非最终 `linker.x`，供外层镜像追加 section |
| `lockdep` | runtime 参与的系统层 lockdep wiring，尤其是 `fs` 启用时传播到 `ax-fs-ng/lockdep` |
| `stack-guard-page` | guard page 初始化和诊断路径；承接旧 `ax-feat/stack-guard-page` 的 `ipi` / `multitask` / `paging` 前置条件 |
| `stack-protector` | stack protector runtime hook |

`ax-runtime` 默认依赖 `axklib` 并提供 `axklib::Klib` glue。这不是用户可选择的能力，而是动态平台和驱动基础路径需要的 runtime glue；未启用 `paging` 或 `irq` 时，相关 `mem_iomap` / IRQ 操作应返回 `Unsupported`，而不是让链接符号缺失。

`ax-runtime/fs` 本身表示“文件系统 runtime 栈存在”，不表示具体磁盘格式。当前仓库确实有 `ext4` 和 `fat` 两种文件系统格式，因此具体 rootfs 格式应通过 `ext4` / `fat` 选择。构建可启动 rootfs 时，不应只启用 `fs` 而不启用任何格式 feature。

### API 与用户库层

| Crate | 保留 feature | 说明 |
| --- | --- | --- |
| `ax-api` | `irq`, `ipi`, `alloc`, `paging`, `dma`, `multitask`, `fs`, `net`, `display`, `stubs` | 只控制 Rust API / 类型是否暴露，不负责 runtime 装配 |
| `ax-posix-api` | `smp`, `irq`, `alloc`, `multitask`, `lockdep`, `fd`, `fs`, `net`, `pipe`, `select`, `poll`, `epoll` | `smp` / `lockdep` 影响 build.rs 生成的 C 头文件和 pthread mutex ABI；该层只控制 POSIX API / C ABI 表面 |
| `ax-std` | `std-compat`, `ext-ld`, `smp`, `fp-simd`, `uspace`, `hv`, `irq`, `ipi`, `wake-ipi`, `alloc`, `paging`, `dma`, `tls`, `multitask`, `lockdep`, `task-ext`, `tracepoint-hooks`, `rr`, `cfs`, `stack-guard-page`, `stack-protector`, `fs`, `ext4`, `fat`, `net`, `vsock`, `aic8800`, `dns`, `fd`, `display`, `input`, `usb`, `rtc`, `backtrace`, `dwarf`, `virtio-*` | Rust 应用侧门面；`fs` 打开文件 API 和基础 runtime fs 栈，但不选择具体 rootfs 格式，格式用 `ext4` / `fat`；`xuantie-c9xx` 这类具体 CPU errata / 扩展由平台或系统集成者直接启用 `ax-hal/xuantie-c9xx`，不放入通用应用门面 |
| `ax-libc` | `smp`, `fp-simd`, `irq`, `alloc`, `tls`, `multitask`, `lockdep`, `stack-protector`, `fs`, `ext4`, `fat`, `net`, `fd`, `pipe`, `select`, `poll`, `epoll` | C 应用侧门面；通过 `ax-posix-api` 暴露 API，通过 `ax-runtime` 装配系统能力 |

## 目标归属模型

### 1. 模块局部 Feature

每个底层 crate 只拥有本 crate 的局部能力。

| Crate | 负责的 feature |
| --- | --- |
| `ax-hal` | `smp`, `irq`, `ipi`, `fp-simd`, `xuantie-c9xx`, `rtc`, `paging`, `tls`, `uspace`, `hv` |
| `ax-task` | `multitask`, `irq`, `ipi`, `preempt`, `smp`, `stack-guard-page`, `task-ext`, `tls`, `tracepoint-hooks`, `uspace`, `rr`, `cfs`, `lockdep` |
| `ax-sync` | `multitask`, `lockdep` |
| `ax-mm` | `copy` |
| `ax-runtime` | runtime 初始化和系统装配 feature |
| `ax-driver` | 具体设备、总线和 probe feature |
| `ax-fs-ng` | `vfs`, `fat`, `ext4`, `lockdep` 等文件系统实现 / API feature |
| `ax-net` | `vsock` 等网络栈 feature |
| `ax-display` | display 子系统 feature |
| `ax-input` | input 子系统 feature |
| `axbacktrace` | `alloc`, `dwarf` |

规则：crate 不应暴露只用于命名其他层产物概念的 feature。

### 2. Runtime 装配 Feature

`ax-runtime` 拥有系统装配图。只要启用某项能力需要启动期初始化、全局 runtime 状态、设备探测、IRQ 注册、DMA 设置或调度器参与，对应依赖就应放在 `ax-runtime`。

目标 `ax-runtime` feature 职责：

| Feature | 含义 |
| --- | --- |
| `irq` | runtime 中断处理和 driver IRQ 集成 |
| `ipi` | 完整 IPI 队列 / 回调支持，同时包含 IRQ、HAL、task 的 IPI wiring |
| `wake-ipi` | 轻量 idle wake IPI handler |
| `smp` | runtime SMP 启动和 task SMP wiring |
| `uspace` | runtime user address space wiring，同时启用 `ax-hal/uspace` 和 `ax-task?/uspace` |
| `paging` | runtime 页表和内存管理支持 |
| `multitask` | runtime task scheduler 集成 |
| `tls` | runtime TLS 设置 |
| `fs` | runtime block driver、VFS/rootfs、DMA、IRQ、multitask 前置条件 |
| `ext4` | runtime ext4 rootfs 支持 |
| `fat` | runtime FAT rootfs 支持 |
| `net` | runtime 网络驱动和网络栈注册；自带 `irq`、`multitask`、`paging` 前置条件 |
| `vsock` | runtime vsock 支持 |
| `display` | runtime display 设备注册；保留旧 `ax-feat/display` 的 `paging` 前置条件 |
| `input` | runtime input 设备注册；保留旧 `ax-feat/input` 的 `paging` 前置条件 |
| `rtc` | runtime RTC 设备支持 |
| `aic8800` | runtime SG2002 AIC8800 Wi-Fi bring-up |
| `std-compat` | Rust std 兼容 runtime hook |
| `ext-ld` | 生成可扩展链接脚本 `runtime.x` 而非最终 `linker.x`，供外层镜像追加 section |
| `lockdep` | runtime 参与的系统层 lockdep wiring，尤其是 `fs` 启用时传播到 `ax-fs-ng/lockdep` |
| `stack-guard-page` | runtime guard-page 初始化；自带 `ipi`、`multitask`、`paging` 前置条件 |
| `stack-protector` | runtime stack protector hook |

规则：`ax-runtime` 可以依赖更底层模块。更底层模块不能依赖 user library 或 API facade crate。

### 3. API 门面 Feature

`ax-api` 和 `ax-posix-api` 只描述哪些公开 API 被编译。它们可以启用类型检查和 API 实现所需的直接依赖，但不能再通过全局 feature 聚合层转发。

`ax-api` 目标 feature：

| Feature | 清理后的直接展开 |
| --- | --- |
| `irq` | `ax-task?/irq`；只保持 API 行为和 `WaitQueue::wait_timeout*` 编译所需 task IRQ 能力，不启用 `ax-runtime/irq` |
| `ipi` | `dep:ax-ipi` |
| `alloc` | `dep:ax-alloc` |
| `paging` | `dep:ax-mm` |
| `dma` | `dep:ax-dma` |
| `multitask` | `ax-task/multitask`, `ax-sync/multitask` |
| `fs` | `dep:ax-fs-ng` |
| `net` | `dep:ax-net` |
| `display` | `dep:ax-display` |
| `stubs` | 原 `dummy-if-not-enabled`，用于在真实 API feature 未启用时生成 stub 符号 |

`ax-posix-api` 目标 feature：

| Feature | 清理后的直接展开 |
| --- | --- |
| `smp` | 空展开；保留该 feature 让 build.rs 在计算 `pthread_mutex_t` 内存布局时区分 SMP 与非 SMP，并作为 bindgen 的 `AX_CONFIG_SMP` 输入 |
| `irq` | 仅当 POSIX API 代码确实需要 IRQ 符号时保留直接依赖 |
| `alloc` | `dep:ax-alloc` |
| `multitask` | `alloc`, `ax-task/multitask`, `ax-sync/multitask` |
| `lockdep` | `multitask`, `ax-sync/lockdep`, `ax-kspin/lockdep`；同时被 build.rs 用于 `pthread_mutex_t` layout 计算，保证 C ABI 与 Rust 锁布局一致 |
| `fd` | `alloc`, `dep:scope-local` |
| `fs` | `dep:ax-fs-ng`, `fd` |
| `net` | `dep:ax-net`, `fd` |
| `pipe` | `fd` |
| `select` | `fd` |
| `poll` | `fd` |
| `epoll` | `fd` |

规则：API 门面的 feature 名可以和 runtime feature 名相同，但 API 门面不负责启用 runtime 装配。

### 4. 用户库 Feature

`ax-std` 和 `ax-libc` 成为唯一的 ArceOS 应用侧便利层。

`ax-std` feature 策略：

- Rust 应用选择 `ax-std/*`。
- `ax-std` 显式组合以下内容：
  - 通过 `ax-api/*` 控制 API 可见性。
  - 通过 `ax-posix-api/*` 控制 POSIX 兼容接口。
  - 通过 `ax-runtime/*` 控制 runtime 装配。
  - 只有 `ax-std` 自身直接使用底层符号时，才直接启用底层模块 feature。
  - 只有历史上 `ax-std` 已承诺的默认应用驱动选择，才保留在 `ax-std` 中。

目标 `ax-std` 映射：

| Feature | 清理后的直接展开 |
| --- | --- |
| `std-compat` | `ax-alloc/global-allocator`, `ax-runtime/std-compat` |
| `smp` | `ax-runtime/smp`, `ax-kspin/smp`, `ax-posix-api/smp` |
| `fp-simd` | `ax-hal/fp-simd` |
| `uspace` | `ax-runtime/uspace` |
| `hv` | `ax-hal/hv` |
| `irq` | `ax-api/irq`, `ax-posix-api/irq`, `ax-runtime/irq` |
| `ipi` | `ax-api/ipi`, `ax-runtime/ipi` |
| `wake-ipi` | `ax-runtime/wake-ipi` |
| `ext-ld` | `ax-runtime/ext-ld` |
| `alloc` | `ax-api/alloc`, `ax-io/alloc`, `ax-posix-api/alloc`；`ax-alloc` 仍作为普通依赖存在，不写 `dep:ax-alloc`，除非后续将该依赖 optional 化 |
| `paging` | `alloc`, `ax-runtime/paging` |
| `dma` | `ax-api/dma`, `ax-runtime/paging` |
| `tls` | `ax-runtime/tls` |
| `multitask` | `ax-api/multitask`, `ax-posix-api/multitask`, `ax-runtime/multitask` |
| `lockdep` | `multitask`, `ax-posix-api/lockdep`, `ax-runtime/lockdep` |
| `task-ext` | `ax-task/task-ext` |
| `tracepoint-hooks` | `ax-task/tracepoint-hooks` |
| `rr` | `irq`, `multitask`, `ax-task/rr` |
| `cfs` | `irq`, `multitask`, `ax-task/cfs` |
| `stack-guard-page` | `ax-runtime/stack-guard-page` |
| `stack-protector` | `ax-runtime/stack-protector` |
| `fs` | `ax-api/fs`, `ax-posix-api/fs`, `ax-runtime/fs`, `ax-driver/virtio-blk`, `fd` |
| `ext4` | `fs`, `ax-runtime/ext4` |
| `fat` | `fs`, `ax-runtime/fat` |
| `net` | `ax-api/net`, `ax-posix-api/net`, `ax-runtime/net`, `ax-driver/virtio-net`, `fd` |
| `vsock` | `ax-runtime/vsock` |
| `aic8800` | `ax-runtime/aic8800` |
| `dns` | `net`；仅改变 `ToSocketAddrs` 是否调用 DNS 查询，不单独启用新的底层模块 |
| `fd` | `ax-posix-api/fd`, `ax-posix-api/poll` |
| `display` | `ax-api/display`, `ax-runtime/display` |
| `input` | `ax-runtime/input` |
| `usb` | `irq`, `ax-driver/usb` |
| `rtc` | `ax-runtime/rtc` |
| `backtrace` | `axbacktrace/alloc` |
| `dwarf` | `axbacktrace/dwarf` |
| `virtio-blk` | `ax-driver/virtio-blk` |
| `virtio-net` | `ax-driver/virtio-net` |
| `virtio-gpu` | `ax-driver/virtio-gpu` |
| `virtio-input` | `ax-driver/virtio-input` |
| `virtio-socket` | `ax-driver/virtio-socket` |

`ax-libc` feature 策略：

- C 应用选择 `ax-libc/*`。
- `ax-libc` 直接组合 POSIX API 和 runtime 装配。
- `ax-libc` 不能依赖 `ax-std`。

目标 `ax-libc` 映射：

| Feature | 清理后的直接展开 |
| --- | --- |
| `smp` | `ax-runtime/smp`, `ax-kspin/smp`, `ax-posix-api/smp` |
| `fp-simd` | `ax-hal/fp-simd` |
| `irq` | `ax-posix-api/irq`, `ax-runtime/irq` |
| `alloc` | `ax-posix-api/alloc` |
| `tls` | `alloc`, `ax-runtime/tls` |
| `multitask` | `ax-posix-api/multitask`, `ax-runtime/multitask` |
| `lockdep` | `ax-posix-api/lockdep`, `ax-runtime/lockdep` |
| `stack-protector` | `ax-runtime/stack-protector` |
| `fs` | `ax-posix-api/fs`, `ax-runtime/fs`, `fd` |
| `ext4` | `fs`, `ax-runtime/ext4` |
| `fat` | `fs`, `ax-runtime/fat` |
| `net` | `ax-posix-api/net`, `ax-runtime/net`, `fd` |
| `fd` | `ax-posix-api/fd` |
| `pipe` | `ax-posix-api/pipe` |
| `select` | `ax-posix-api/select` |
| `poll` | `ax-posix-api/poll` |
| `epoll` | `ax-posix-api/epoll` |

规则：`ax-std` 和 `ax-libc` 应保持显式，但不要重复展开 `ax-runtime` 已经负责的下层细节。它们的 feature 表应让组合关系清晰可审计。

### 5. 上层系统 Feature

StarryOS 和 Axvisor 不能再依赖 `ax-feat`。

StarryOS 目标策略：

- `starry-kernel` 直接启用所需 ArceOS 基础模块：
  - `ax-hal/*`
  - `ax-runtime/*`
  - `ax-task/*`
  - `ax-sync/*`
  - `ax-driver/*`
  - `ax-fs-ng/*`
  - `ax-net/*`
  - `axbacktrace/*`
- `starry-kernel` 如果直接使用 `ax_sync::Mutex` 等 sleepable / task-aware 同步原语，应在自身依赖上启用 `ax-sync/multitask`，不能依赖 `starryos` 镜像包或其他上层组合“碰巧”打开。
- `starryos` 镜像包主要启用 `starry-kernel/*`、平台和具体驱动。
- LKM crate 依赖与主 StarryOS kernel image 相同的直接底层 feature，不能再导入单独的 feature 聚合路径。

Axvisor 目标策略：

- 只有在构建 ArceOS 风格用户库接口时，才使用 `ax-std/*`。
- hypervisor / platform 装配使用直接底层 feature：
  - `ax-hal/hv`
  - `ax-runtime/*`
  - `ax-task/*`
  - `ax-driver/*`

规则：上层系统应表现为系统集成者，而不是应用 crate。

## 具体清理步骤

### 步骤 1：冻结当前 Feature 图

改动前先生成基线：

```bash
cargo metadata --no-deps --format-version 1 > tmp/axfeat-cleanup-metadata-before.json
rg -n "ax-feat|axfeat" . > tmp/axfeat-cleanup-refs-before.txt
```

把每个当前 `ax-feat` 引用归入以下类别之一：

- `ax-std` 映射。
- `ax-libc` 映射。
- `ax-api` API 可见性。
- `ax-posix-api` API 可见性。
- `ax-runtime` 装配。
- StarryOS 系统装配。
- Axvisor 系统装配。
- axbuild feature 解析。
- docs / changelog / 生成的组件文档。

### 步骤 2：让 API Crate 脱离 `ax-feat`

编辑：

- `os/arceos/api/arceos_api/Cargo.toml`
- `os/arceos/api/arceos_posix_api/Cargo.toml`

必要改动：

- 删除 `ax-feat.workspace = true`。
- 将所有 `ax-feat/*` feature 展开替换为上文 API 映射中的直接本地依赖。
- 保持 optional dependency 仍为 optional。
- 除非 API 代码确实调用 runtime 符号，否则不要从 API crate 启用 `ax-runtime/*`。
- 删除 `arceos_api/stack-guard-page` feature；当前 `arceos_api/src` 没有对应 API 符号，stack guard page 的真实处理在 `ax-runtime` 和 `ax-task`。
- 将 `dummy-if-not-enabled` 重命名为 `stubs`，避免使用描述实现细节的长 feature 名。

期望结果：

```bash
rg -n "ax-feat|axfeat" os/arceos/api/arceos_api os/arceos/api/arceos_posix_api
```

除非历史 changelog 按仓库策略需要保留，否则不应再有匹配。

### 步骤 3：收敛底层模块和 `ax-runtime` Feature

编辑：

- `os/arceos/modules/axtask/Cargo.toml`
- `os/arceos/modules/axfs-ng/Cargo.toml`
- `os/arceos/modules/axfs-ng/src/file/handle.rs`
- `os/arceos/modules/axfs-ng/src/fs/ext4/rsext4/*`
- `os/arceos/modules/axruntime/Cargo.toml`

必要改动：

- 删除 `ax-task/sched-fifo` 和 `ax-std/sched-fifo`。FIFO 是 `multitask` 未选择 `rr` / `cfs` 时的默认调度器，不再作为 feature。
- 将 `ax-task/sched-rr`、`ax-task/sched-cfs` 分别重命名为 `ax-task/rr`、`ax-task/cfs`。这是调度算法自身的真实名字，`sched-` 前缀在 `ax-task` 命名空间内属于冗余。
- 删除 `ax-task/stack-canary` 独立 feature，并让 task stack canary 成为 `multitask` 的默认实现细节。
- 保留 `ax-runtime/ext-ld`，因为它通过 `CARGO_FEATURE_EXT_LD` 控制 `axruntime/build.rs` 输出 `runtime.x` 还是 `linker.x`。不要引入 `dynld` 新名，避免把 external linker 误读为 dynamic loader。
- 将 `ax-runtime/aic8800-wifi` 和 `ax-driver/aic8800-wifi` 重命名为 `aic8800`，并保持 runtime 侧自带 `net`、`ax-driver/aic8800`、`dep:aic8800`、`dep:sdhci-cv1800` 等真实 Wi-Fi bring-up 依赖。
- `ax-runtime` 默认依赖 `axklib` 并编译 `axklib::Klib` glue。动态平台基础路径会无条件使用 `axklib::mmio`，因此 `axklib` 不能只挂在 `paging`、`fs` 或 `net` 后面；未启用 `paging` / `irq` 时对应操作返回 `Unsupported`。
- 删除 `dma = ["paging"]`。
- 将 `fs` feature 中的 `"dma"` 替换为 `"paging"`，避免 `fs` 引用已删除的 runtime feature。
- 保留 `fs` feature 中的 `"ax-fs-ng/vfs"`，因为它控制 `ax-fs-ng` 高层 VFS / cache / open-options / context API 是否编译。
- 删除 `ax-fs-ng/times` feature，并把当前 `#[cfg(feature = "times")]` 门控改为默认编译。文件元数据、POSIX stat 字段、`utime` / `utimensat` 语义都天然需要时间戳；无 wall time provider 时保留当前返回 0 的运行时回退即可。
- 新增 `lockdep = ["ax-fs-ng?/lockdep"]`，让启用 `lockdep + fs` 时文件系统层锁也进入 lockdep 诊断。
- 新增 `uspace = ["ax-hal/uspace", "ax-task?/uspace"]`，承接旧 `ax-feat/uspace`，避免只启用 HAL 而丢失 `TaskControlBlock::switch_page_table()` 等 task 层 API。
- 将 `stack-guard-page` 增强为 `["ipi", "multitask", "paging", "ax-task?/stack-guard-page"]`，承接旧 `ax-feat/stack-guard-page` 的完整前置条件，避免 StarryOS 等直接启用 runtime feature 时丢失 IPI TLB shootdown 路径。
- 将 `net` 改为自包含系统装配 feature，至少包含：

```toml
net = [
  "irq",
  "multitask",
  "paging",
  "dep:ax-net",
  "dep:rd-net",
  "dep:spin",
  "ax-driver/net",
]
```

- 将 `display` 和 `input` 补上 `paging`，保持旧 `ax-feat/display`、`ax-feat/input` 的前置条件语义：

```toml
display = ["paging", "dep:ax-display", "ax-driver/display"]
input = ["paging", "dep:ax-input", "ax-driver/input"]
```

- 增强 `ipi` feature，使它承接旧 `ax-feat/ipi` 的完整 wiring：

```toml
ipi = ["irq", "dep:ax-ipi", "ax-hal/ipi", "ax-task?/ipi"]
```

这样 `ax-std/ipi` 和上层系统只需要启用 `ax-runtime/ipi`，不会丢失 HAL/task IPI 路径。

### 步骤 4：让 `ax-std` 脱离 `ax-feat`

编辑：

- `os/arceos/ulib/axstd/Cargo.toml`
- `os/arceos/ulib/axstd/src/lib.rs`

必要改动：

- 删除 `ax-feat.workspace = true`。
- 将每个 `ax-feat/*` 展开替换为直接的 `ax-runtime/*`、`ax-hal/*`、`ax-task/*`、`ax-sync/*`、`ax-driver/*`、`axbacktrace/*` 或 API 门面 feature。
- 更新 crate 文档，删除 “`ax-std` features are exactly the same as those in `ax-feat`” 之类描述。
- 保持 `ax-std` 作为 ArceOS Rust 应用 feature 门面。
- `rr` / `cfs` 必须组合 `irq` 和 `multitask`，承接旧 `ax-feat/sched-rr` / `ax-feat/sched-cfs` 的中断前置条件。
- 这是有意的行为改进，不是等价机械替换：旧 `ax-feat/sched-*` 只会打开 `ax-task` 的调度算法 feature，可能缺少 runtime scheduler 初始化和用户侧 API；清理后用户启用 `ax-std/rr` 或 `ax-std/cfs` 应得到完整可运行的 multitask runtime 栈。FIFO 不需要独立 feature，启用 `multitask` 且不选择 `rr` / `cfs` 即为 FIFO。

重要规则：

- 不要为了缩短 feature 列表而创建 `base-fs`、`runtime-net` 之类辅助别名。显式组合更容易审计。
- 不要在 `ax-std` 重复展开 `ax-runtime` 已经负责的 `ax-hal`、`ax-task`、`ax-driver` 细节，除非 `ax-std` 自身直接使用对应符号。

### 步骤 5：让 `ax-libc` 脱离 `ax-feat`

编辑：

- `os/arceos/ulib/axlibc/Cargo.toml`

必要改动：

- 删除 `ax-feat.workspace = true`。
- 新增 `ax-runtime.workspace = true` 和 `ax-kspin.workspace = true`，因为清理后的 `ax-libc` feature 会直接引用 `ax-runtime/*` 和 `ax-kspin/smp`。
- 将 `fp-simd`、`irq`、`tls`、`multitask`、`lockdep`、`stack-protector` 映射替换为直接模块 / runtime 映射。
- 保持 libc 专属 feature（`fd`、`pipe`、`select`、`poll`、`epoll`）通过 `ax-posix-api` 路由。
- 新增 `poll = ["ax-posix-api/poll"]`；当前 `ax-posix-api/poll` 已存在，但 `ax-libc` 尚未暴露同名 feature。
- 新增 `ext4`、`fat` 两个 C 应用侧 feature，并分别映射到 `fs + ax-runtime/ext4`、`fs + ax-runtime/fat`。

### 步骤 6：让 StarryOS 脱离 `ax-feat`

至少编辑：

- `os/StarryOS/kernel/Cargo.toml`
- `os/StarryOS/starryos/Cargo.toml`
- `os/StarryOS/lkm/*/Cargo.toml`
- `os/StarryOS/lkm/README.md`
- `os/StarryOS/docs/*` 中提到 `ax-feat` 的引用。
- `apps/starry/*` 中提到 `ax-feat` 的文档或构建配置。

目标替换示例：

| 旧写法 | 新写法 |
| --- | --- |
| `ax-feat/input` | `ax-runtime/input`，如果代码直接使用 `ax-input` API，再加 `dep:ax-input` |
| `ax-feat/ipi` | `ax-runtime/ipi`，如果 kernel 代码直接使用 IPI API，再加 `dep:ax-ipi` |
| `ax-feat/backtrace` | `axbacktrace/alloc` |
| `ax-feat/fp-simd` | `ax-hal/fp-simd` |
| `ax-feat/xuantie-c9xx` | `ax-hal/xuantie-c9xx` |
| `ax-feat/uspace` | `ax-runtime/uspace` |
| `ax-feat/multitask` | `ax-runtime/multitask`，如果 kernel 代码直接使用 task API，再加 `ax-task/multitask` |
| `ax-feat/task-ext` | `ax-task/task-ext` |
| `ax-feat/tracepoint-hooks` | `ax-task/tracepoint-hooks` |
| `ax-feat/sched-fifo` | 删除，无替代 feature；启用 `ax-runtime/multitask` 且不启用 `rr` / `cfs` 即为 FIFO |
| `ax-feat/sched-rr` | `ax-runtime/irq`, `ax-runtime/multitask`, `ax-task/rr` |
| `ax-feat/sched-cfs` | `ax-runtime/irq`, `ax-runtime/multitask`, `ax-task/cfs` |
| `ax-feat/ext4fs` | `ax-runtime/ext4` |
| `ax-feat/fatfs` | `ax-runtime/fat` |
| `ax-feat/net` | `ax-runtime/net`，如果 kernel 代码直接使用 `ax-net` 符号，保留直接依赖 `ax-net` |
| `ax-feat/vsock` | `ax-runtime/vsock`，如果 kernel 代码直接使用 net 符号，再加 `ax-net/vsock` |
| `ax-feat/usb` | `ax-runtime/irq`, `ax-driver/usb` |
| `ax-feat/aic8800-wifi` | `ax-runtime/aic8800` |
| `ax-feat/stack-guard-page` | `ax-runtime/stack-guard-page`，并保留 Starry 诊断 feature |
| `ax-feat/stack-protector` | `ax-runtime/stack-protector` |
| `ax-feat/fs-times` | 删除，无替代 feature；文件时间戳默认启用 |
| `ax-feat/ext-ld` | `ax-runtime/ext-ld`；若通过 `ax-std` 入口构建，则使用 `ax-std/ext-ld` |
| `ax-feat/rtc` | `ax-runtime/rtc` |
| `ax-feat/display` | `ax-runtime/display` |
| `ax-feat/irq` | `ax-runtime/irq` |
| `ax-feat/smp` | 对用户侧门面映射为 `ax-runtime/smp` + `ax-kspin/smp` + `ax-posix-api/smp`；对内部 crate 则按实际消费的 SMP 能力直接启用对应模块 feature |

StarryOS 清理后的依赖策略：

- `starry-kernel` 应显式依赖自己消费的 ArceOS 模块。
- `starryos` 不应启用底层 ArceOS feature，除非它拥有该 feature 的镜像级集成职责。
- LKM 示例应直接镜像主 StarryOS kernel 的 feature 表面。

### 步骤 7：让 Axvisor 脱离间接 `ax-feat`

编辑：

- `os/axvisor/Cargo.toml`
- `virtualization/*/Cargo.toml` 中仍存在 `ax-feat` 引用的文件。
- 提到 `ax-feat` 的 Axvisor 文档和构建测试。

如果 Axvisor 只通过 `ax-std` 间接接收 `ax-feat`，则本步骤主要是在 `ax-std` 清理后做验证。

同时替换旧文件系统格式 feature 名：

- `ax-std/ext4fs` 改为 `ax-std/ext4`。
- `ax-std/fatfs` 改为 `ax-std/fat`。

### 步骤 8：从 Workspace 删除 `ax-feat`

编辑：

- 根目录 `Cargo.toml`。
- 任何 workspace package 列表或 dependency table 中的 `ax-feat` 条目。
- release / changelog 工具中枚举 package 的位置。
- std-test whitelist 中的 `ax-feat` 条目。
- 如果仓库提交了生成文档索引，也要同步清理。

删除：

- `os/arceos/api/axfeat/Cargo.toml`
- `os/arceos/api/axfeat/src/lib.rs`
- `os/arceos/api/axfeat/CHANGELOG.md`
- `os/arceos/api/axfeat/`

不要添加替代 package。

### 步骤 9：删除 `axbuild` 中的 `ax-feat` 支持

编辑：

- `scripts/axbuild/src/build/platform.rs`
- `scripts/axbuild/src/build/info.rs`
- `scripts/axbuild/src/build/std_build.rs`
- `scripts/axbuild/src/arceos/cbuild/features.rs`
- `scripts/axbuild/src/starry/build.rs`
- `scripts/axbuild/src/axvisor/build/features.rs`
- 所有引用 `ax-feat` 或 `axfeat` 的 axbuild 测试。

清理后的必要行为：

- `FEATURES=axfeat/net` 非法。
- `FEATURES=ax-feat/net` 非法。
- Build Info 不再自动检测 `AxFeat` 前缀族。
- 应用包必须依赖 `ax-std`、`ax-libc` 或直接依赖更底层模块。
- `axbuild` 不再归一化 `axfeat` 别名。
- `axbuild` 不再剥离 `ax-feat/` 前缀。
- `axbuild` 不再把 `ext4fs` / `fatfs` 当作可继续传播的旧 feature 名；构建配置必须使用 `ext4` / `fat`。
- C app feature 映射不再把未知低层系统 feature 回退到 `ax-feat/{unknown}`，而应直接映射到模块 / runtime feature 或拒绝。

删除：

- `AxFeaturePrefixFamily::AxFeat`
- `normalize_legacy_feature_alias()` 中对 `axfeat` 的处理。
- `is_removed_dynamic_platform_feature()` 中对 `ax-feat/plat-dyn` 的分支。
- `feature_family_from_existing_features()` 对 `ax-feat/` 的检测。
- `detect_ax_feature_prefix_family()` 对直接依赖 `ax-feat` 的接受逻辑。

简化目标：

```rust
enum StdFeaturePrefixFamily {
    AxStd,
}
```

如果该 enum 已经没有实际用途，则直接删除整个 enum。若为了保持现有解析流程清晰而暂时保留单变体 `StdFeaturePrefixFamily::AxStd`，也可接受；关键是不再存在 `AxFeat` 分支或 `axfeat` / `ax-feat` 兼容路径。

### 步骤 10：清理文档

编辑所有把 `ax-feat` 描述为架构层的文档：

- `docs/docs/architecture/arceos.md`
- `docs/docs/development/arceos.md`
- `docs/docs/development/components.md`
- `docs/docs/components/layers.md`
- `docs/docs/components/crates/ax-feat.md`
- `docs/docs/components/crates/ax-api.md`
- `docs/docs/components/crates/ax-posix-api.md`
- `docs/docs/components/crates/ax-std.md`
- `docs/docs/components/crates/ax-runtime.md`
- `docs/docs/components/crates/starry-kernel.md`
- `docs/docs/components/crates/starryos.md`
- `docs/docs/build/arceos/build.md`
- `docs/docs/debug/task-stack-guard-page.md`
- `docs/docs/debug/check-mechanisms-summary.md`
- `os/arceos/doc/build.md`

必要文档改动：

- 从架构图中移除 `ax-feat`。
- 删除 `docs/docs/components/crates/ax-feat.md`。
- 将“API 聚合层包含 axfeat”替换为：
  - `ax-api`：Rust API 门面。
  - `ax-posix-api`：POSIX API 门面。
  - `ax-std` / `ax-libc`：应用侧用户库。
  - `ax-runtime`：系统装配和初始化。
- 记录直接 feature 归属规则。
- 根据上下文，将构建示例中的 `ax-feat/*` 改为 `ax-std/*`、`ax-libc/*`、`ax-runtime/*` 或直接模块 feature。

### 步骤 11：清理测试和生成产物

编辑：

- `scripts/axbuild/src/build/tests/*`
- `scripts/axbuild/src/arceos/build/tests.rs`
- `scripts/axbuild/src/arceos/cbuild/tests.rs`
- `scripts/axbuild/src/starry/test/tests/*`
- `scripts/axbuild/src/test/std.rs`
- `test-suit/arceos/rust/Cargo.toml` 中的 `ax-std/ext4fs` / `ax-std/fatfs` 旧 feature 名。
- feature 列表中包含 `ax-feat` 的 `test-suit/*` 配置。

测试更新必须断言新的行为：

- `axfeat/*` 不会被归一化。
- `ax-feat/*` 会被拒绝，或只有在所选 package 真实存在同名 feature 时才原样保留；由于 `ax-feat` 已删除，普通 ArceOS 流程应拒绝它。
- feature 前缀族检测不再接受 `ax-feat`。
- C app 映射不再回退生成 `ax-feat/{unknown}`。
- Starry build 配置不再包含 `ax-feat/plat-dyn`。
- 测试和示例不再使用 `ax-std/ext4fs` / `ax-std/fatfs`，统一使用 `ax-std/ext4` / `ax-std/fat`。

### 步骤 12：最终搜索必须干净

完成代码和文档清理后执行：

```bash
rg -n "ax-feat|axfeat|ax_feat|AxFeat|axfeat/" .
```

允许的匹配：

- 如果仓库策略要求历史 changelog 不可变，则只允许历史 changelog 条目。
- 本方案文件，直到方案被删除或归档。

除此之外，所有匹配都必须删除或改写。

## 清理后的依赖规则

硬性规则：

- `os/arceos/api/*` 不能依赖已删除的 feature 聚合 crate。
- `ax-runtime` 不能依赖 `ax-api`、`ax-posix-api`、`ax-std` 或 `ax-libc`。
- `ax-api` 和 `ax-posix-api` 不能启用 `ax-runtime/*`，除非它们直接调用 runtime 符号。
- `ax-std` 可以依赖 `ax-api`、`ax-posix-api`、`ax-runtime` 和更底层模块。
- `ax-libc` 可以依赖 `ax-posix-api`、`ax-runtime` 和更底层模块。
- StarryOS 和 Axvisor 在装配 kernel / runtime 内部能力时，不能使用 ArceOS user-library feature 名；只有明确链接 user library 表面时例外。
- 上层 `Cargo.toml` 依赖 `ax-*` crate 时，如不希望被默认 feature 影响，应显式写 `default-features = false`，然后只开启本镜像实际需要的 feature。

期望依赖方向：

```text
apps / C apps
  -> ax-std / ax-libc
  -> ax-api / ax-posix-api
  -> ax-runtime
  -> ax-hal / ax-task / ax-sync / ax-driver / ax-fs-ng / ax-net / ...
  -> components / platforms

StarryOS / Axvisor
  -> ax-runtime and lower modules directly
```

## 验证计划

先运行格式化：

```bash
cargo fmt
```

运行定向 clippy：

```bash
cargo xtask clippy --package ax-runtime
cargo xtask clippy --package ax-api
cargo xtask clippy --package ax-posix-api
cargo xtask clippy --package ax-std
cargo xtask clippy --package ax-libc
cargo xtask clippy --package starry-kernel
cargo xtask clippy --package starryos
cargo xtask clippy --package axbuild
```

运行 axbuild 测试：

```bash
cargo test -p axbuild
```

当前 `cargo xtask test` 子命令不支持 `--package` 过滤；因此 axbuild 单元测试使用原生 Cargo 的 `cargo test -p axbuild`。这属于 xtask 无法表达该精确配置时的等价验证命令。

运行代表性 ArceOS 构建：

```bash
cargo xtask arceos build --package arceos-helloworld --arch aarch64
cargo xtask arceos build --package arceos-thread-test --arch aarch64
cargo xtask arceos build --package arceos-fs-shell --arch aarch64
cargo xtask arceos build --package arceos-net-httpserver --arch aarch64
```

运行代表性 StarryOS 构建：

```bash
cargo xtask starry build --arch riscv64
cargo xtask starry build --arch aarch64
```

运行代表性 Axvisor 构建：

```bash
cargo xtask axvisor build --arch aarch64
```

如果某条命令不符合当前 xtask 语法，先查看 `cargo xtask --help`，使用最接近的 xtask 支持命令。除非 xtask 无法表达对应配置，否则不要对 OS 镜像构建回退到原生 Cargo 命令。

## 审查清单

合并前检查：

- `ax-feat` 已从根 workspace dependencies 中移除。
- `os/arceos/api/axfeat` 目录已删除。
- 活跃代码路径中不再引用 `ax-feat`、`axfeat`、`AxFeat` 或 `ax_feat`。
- `axbuild` 中不再存在 `ax-feat` 前缀族。
- C app feature 映射不再生成 `ax-feat/*`。
- `ax-std` feature 定义显式且可读。
- `ax-libc` feature 定义显式且可读。
- `ax-api` 和 `ax-posix-api` 不再承担 runtime 装配职责。
- `ax-runtime` 拥有系统装配依赖。
- StarryOS 使用直接 runtime / module feature。
- Axvisor 在合适位置使用直接 runtime / module feature。
- 文档不再把 `ax-feat` 描述为 ArceOS 层级。
- 所有定向 clippy 检查通过。
- 代表性 ArceOS、StarryOS 和 Axvisor 构建通过。

## 期望最终状态

清理完成后，feature 模型应很容易解释：

- 构建 Rust ArceOS 应用：使用 `ax-std/*`。
- 构建 C ArceOS 应用：使用 `ax-libc/*`。
- 暴露 Rust API：使用 `ax-api/*`。
- 暴露 POSIX API：使用 `ax-posix-api/*`。
- 需要 runtime 初始化：使用 `ax-runtime/*`。
- 做 kernel / system 集成：直接依赖需要的具体模块。

不再存在单独的顶层 feature crate。Cargo feature 图本身就是唯一事实来源。
