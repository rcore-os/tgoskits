# RK3588 客户机任务切换测量契约

## 1. 测量对象

测量对象是 AxVisor 上 ArceOS 客户机的任务切换。`apps/arceos/task-switch` 的 `bench_switch()` 测量主任务与 peer 的 yield 往返，`virtualization/axvm/tests/task_switch.rs` 负责客户机生命周期和输出完整性；这与宿主 vCPU 的调度切换不是同一个指标。

### 1.1 工作量与单位

`SWITCH_ROUNDS = 30`、`SWITCHES_PER_ROUND = 10_000_000` 保持不变。每个采样区间包含两次切换，PMU 周期和系统计数器时间按两次切换均摊。`TASK_SWITCH_SUMMARY` 的 `avg_cycles` 是平均每次切换的周期数，800 周期目标按此量判定；逐轮均值和最小、最大值必须同时保留，平均达标不代表最坏时延小于 800 周期。

`AXTEST_SUITE_OK` 目前只证明客户机完成了约定工作量并给出有效测量，不包含性能上限承诺。报告必须把功能通过与性能达标分开，不能以成功标记替代周期数据。

### 1.2 计时边界

`bench_switch()` 的区间包含客户机调度、原子交接以及 peer 对 GPIO3_C6 的低电平写入；主任务的高电平写入位于计时终点之后。`cycle::cpu_cycle()` 的读数屏障也是观测路径的一部分。原始指标不扣除仪器成本，不删除 GPIO 信号，不改变往返次数定义。

分层诊断可以单独测量 GPIO、计数器读取与调度阶段，但必须使用独立名称记录，不能将扣除开销或插桩后的数据写成原始完整基准结果。GPIO 访问若作等价修复，保持引脚、信号、计时位置及其他引脚不受影响的契约，单独说明仪器成本变化。

## 2. 调度契约

客户机主任务与 peer 使用 `SchedulePolicy::fifo(RtPriority(80))`；宿主通过 `VmCreatePlan::vcpu_schedule_policy` 独立配置 vCPU 任务。两个层次的策略不能互相推断。`cpu_num = 1` 和 `max_cpu_num = 1` 也不等于最终产物关闭了编译期 SMP 能力，必须核实构建特性。

### 2.1 Linux 参考

调度语义以 Linux v7.1、提交 `8cd9520d35a6c38db6567e97dd93b1f11f185dc6` 为对照。`kernel/sched/rt.c::yield_task_rt()` 仅轮转当前优先级链，之后 `pick_task_rt()` 经全局优先级位图选择最高可运行 RT 任务；不能在存在更高 RT 优先级任务时直接复用当前链的队首。

`kernel/sched/core.c::__schedule()` 的 rq 所有权、时钟更新和 `finish_task_switch()` 的 release-before-unlock 顺序继续约束共享实现。已有契约见 [ax-task RT 审计](../docs/design/ax-task-linux-v7.1-rt-audit.md)。

### 2.2 优化边界

普通 FIFO yield 已采用 linked RT 节点和跨切换 rq baton。任何后续调整都必须保留 task→rq 锁序、唤醒请求不丢失、previous 在裸切换完成后才释放物理所有权、current 发布至裸切换之间不可失败、RT 带宽记账与 RR quantum 等不变量。

配置选择必须对应可解释的部署能力，不按测试名、固定优先级或门槛选择隐藏路径。关闭通用能力的成本与功能影响须明确记录，不能静默改变默认行为。涉及 CPU binding、安全检查或 FPU 策略的契约变化需要单独设计确认，不以性能目标替代健全性证明。

### 2.3 标准库内存复制

本应用使用真实 Rust 标准库，`std::thread::yield_now()` 经 `ax_std::os::libc_compat::sched_yield()`、`ax_posix_api::sys_sched_yield()` 进入调度器，并非直接调用 mini-std 的线程入口。编译器生成的聚合值转移也会调用同一兼容层导出的 `memcpy()`，因此内存复制属于客户机执行路径的一部分。

内存复制由 `os/arceos/ulib/axstd/src/os/libc_compat.rs::memcpy()` 负责，而不是调度器。其既有 C ABI、返回目标地址、不重叠内存复制语义和任意字节对齐输入保持不变；不创建新的公共 API、不分配内存、不改变锁或任务状态。复制必须允许源对象包含未初始化 padding，不把原始存储解释成已经初始化的整数或引用；任何宽标量访问都要求已证明的源、目标对齐与完整长度，不越界读取或写入。零长度不访问内存。不同对齐关系继续使用合法的窄访问，不能通过关闭严格对齐、开启内核 SIMD 或访问 MMIO 获得收益。

Linux v7.1 的 `lib/string.c` 定义正常内存复制契约，`arch/arm64/lib/memcpy.S` 以标量批量转移避免逐字节循环。后者依赖普通内存未对齐访问的实现选择不能直接用于本项目 MMU 前的严格对齐路径。此处复用现有兼容层边界，以符合 Rust 原始存储规则的标量访问实现同一契约；不照搬 Linux 的未对齐访问，也不调整调度算法。

### 2.4 GPIO 单引脚写入

`apps/arceos/task-switch/src/gpio.rs::set_gpio_bit()` 的责任是更新选中引脚，不拥有同 bank 的其他输出。RK3588 GPIO V2 的数据寄存器以高 16 位选择本次写入的有效位，因此单引脚操作应只写对应半区的目标数据位与 write-enable 位，不读回并重写整个 bank。显式 `gpio3_clear_all()` 继续拥有原来的整 bank 清零语义，不能混用两种操作。

Linux v7.1 `drivers/gpio/gpio-rockchip.c` 的 `rockchip_gpio_writel_bit()` 对 V2 采用相同单引脚 masked write。映射存活、32 位易失访问、引脚初始化和原有访问顺序保持不变，不移出计时区间、不删除 GPIO 信号。GPIO 访问属于仪器开销，与通用 `memcpy` 的运行成本分开归因。

### 2.5 固定 CPU 的构建模式

此场景的单 vCPU 固定在 RK3588 的 Cortex-A76 核 `cpu@400`。显式选择 `target-cpu=cortex-a76` 可以让编译器使用该 CPU 的指令能力和代价模型；该镜像不能再当作可迁移到任意 ARMv8-A CPU 的通用产物。此选择只属于客户机构建配置，不传递给在其他物理核上启动的 AxVisor 宿主，也不改变工作区的通用 AArch64 目标 JSON。

性能模式保留 `-Zstack-protector=strong`、`-Ctarget-feature=-neon`、共享目标 JSON 中的 `+strict-align` 和既有 soft-float ABI，并采用裸机路径已经使用的 `-Cllvm-args=--max-store-memcpy=16` 短复制代价策略。后者不改变访问对齐是否合法，也不新增运行期分支，相关约束见 [短复制内联设计](../docs/design/aarch64-inline-copy.md)。不在命令行重复传递会触发不稳定特性警告的 `+strict-align`。

两份客户机构建配置通过根级 `BuildInfo.rustflags` 数组追加 CPU、NEON 与短复制参数，不使用 `[env] RUSTFLAGS` 替换 axbuild 生成的 target 参数。栈保护由 `toolchain_rustflags_for_mmu()` 提供，`DWARF=y` 的调试信息和帧指针、`BACKTRACE=y` 的帧指针保持生效；显式环境覆盖仍遵守 Cargo 的既有优先级。共享接口及兼容边界见 [构建参数追加契约](axbuild-rustflags.md)。

编译器可能改变原子操作的指令选择，但该构建模式不改变 Rust 原子内存序、调度策略、rq 所有权与 IRQ 协议。

## 3. 运行入口

应用与持续集成共用 `axvm` 的 `task_switch` 内核测试入口和客户机测量程序；板卡服务类型与资源路径分别由各自配置维护。

### 3.1 应用入口

`apps/axvisor/normal/board-ballpick-robot/task-switch/` 维护应用板卡的宿主、客户机与启动配置。`guest-builds.toml` 声明宿主之前的客户机构建，`configs/guest-build.toml` 选择客户机能力与代码生成参数，`configs/task-switch.toml` 约束 CPU 绑定和直通资源。

### 3.2 基准入口

`benchmarks/axvisor/board-orangepi-5-plus/task-switch/` 维护持续集成性能入口。其 `guest-build.toml`、`task-switch.toml` 与应用入口保持相同的工作量、客户机构建参数和测量语义；板卡类型及相对资源路径按基准场景配置，不从服务名称推断硬件兼容性。
