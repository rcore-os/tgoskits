# 线程装配与 Linux RT 生命周期

## 1. 语义基准

本次重构对照 `/home/zhourui/linux-src` 的 Linux v7.1 提交 `8cd9520d35a6c38db6567e97dd93b1f11f185dc6`，分析 `CONFIG_PREEMPT_RT=y` 对应路径。该目录现有 `.config` 未启用 RT，不能作为 RT 运行或性能证据。

### 1.1 所有权

`ax-task` 拥有调度状态和公共线程执行生命周期，`ax-runtime` 提供架构资源装配，Starry 的 `Thread` 持有 Linux 每线程状态并引用共享 `ProcessData`。OS 扩展直接安装到 `ThreadSpec`，不经过内核线程或运行时线程的外层扩展转发。公共创建使用 `ThreadBuilder`，运行队列和唤醒句柄保持非泛型。

### 1.2 顺序对照

Linux 顺序是迁移验收条件，不要求复制 C 对象布局。每条路径必须同时检查发布者、观察者和资源释放责任。

| Linux 基准 | 当前入口 | 最终所有者 | 顺序与验证场景 |
| --- | --- | --- | --- |
| `dup_task_struct/copy_process/sched_fork` | `ThreadBuilder`、`create_thread`、`UnpublishedContext` | ax-task 创建事务；ax-runtime 资源装配 | 完整初始化后保持 `New`；ArceOS/Starry QEMU 逐阶段回滚、提前普通 wake |
| `wake_up_new_task` | `PreparedThread::stage`、`StagedThread::activate` | 登记表中的 activation 预留与目标 CPU 接收端 | 身份发布后首次激活；QEMU staged affinity/policy 更新、快速退出、取消不执行入口 |
| `copy_process` 错误退出路径 | `publish_thread_cancellation`、`process_thread_cancellation` | 唯一创建令牌转交任务上下文 reaper | IRQ Drop 不取登记表锁；无管理句柄的取消与扩展一次性析构 |
| `try_to_wake_up/rt_mutex_schedule` | `ThreadLifecycle`、`PiWaitToken` | ax-task 等待状态、PI owner 与 rq | 普通通知和锁交接分离；RT 外层超时、读者排空、远端唤醒与纯状态/Loom |
| `context_switch/finish_task_switch` | `execute_switch_plan`、runtime switch tail | rq 提交；runtime 物理上下文；incoming tail | 读取 prev 终态后 release off-CPU；SMP 切出/迁移及四类 MM 交接 |
| `schedule_tail` | 公共 trampoline | ax-task 公共执行对象与 runtime baton | 首次入口、阻塞恢复和 yield 后验证 IRQ/阻塞许可 |
| `put_task_stack/put_task_struct` | `take_exited_execution`、task-work consumer | 资源、OS extension 和 active-MM 各自的所有者 | 管理句柄不保留退出栈；原子上下文直接 reap 拒绝；lazy-MM 在最后 CPU lease 退出后释放 |

`finish_task_switch` 先读取退出状态，再清除 `on_cpu`；任务引用和内核栈引用具有不同寿命。Linux RT 通过延迟释放避免在原子上下文取得睡眠锁，本项目复用任务上下文 reaper，并单独证明每个被借用对象的读者保护。

### 1.3 锁等待边界

`RawMutex` 保持普通 PI mutex 等待。`SpinLock`、`SpinRwLock` 使用独立的 RT 锁等待语义，原不可睡眠实现改名为 `RawSpinLock`、`RawSpinRwLock`。普通锁、RT 锁和 raw 锁不能仅按接口形状互换：IRQ、调度交接和已有抢占保护中的调用方显式使用 raw 类型。

`ThreadLifecycle` 的一个 `AtomicU16` 同时保存物理调度状态、内层通知和外层状态。`RTLOCK_ACTIVE` 期间，普通唤醒只更新高字节的保存通知；锁交接通过独立 `WakeSource` 更新低字节，才能激活内层阻塞。`PiHandoffWake` 在 wait-lock 内捕获 PI 代次，交接延迟到下一次等待时不会误唤醒新等待。读写锁的读者排空使用独立的内层 park 代次。

外层 `ParkTicket` 的代次保存在 `ThreadCore::ordinary_park_generation`；内层 park 使用单调递增的 `park_sequence`，不能恢复计数器而复用旧代次。硬、软和同步超时到期路径都比较普通等待代次。`restore_rt_lock_wait` 在调度锁下恢复外层状态和票据，外层通知使随后的 commit 取消阻塞，而非重新丢入运行队列。

### 1.4 锁类型与上下文

锁对象的 Rust 借用保护被锁数据的有效期；调度侧迁移 pin、RT 临界区深度和等待状态分别表达不同约束。`RtCriticalGuard` 只禁止普通睡眠，不禁止抢占或另一把 RT 锁的竞争调度。它不是通用 RCU 宽限期实现，不能用其计数或 `Arc` 计数替代第 2.2 节的逐对象读者证明。

| Linux RT 对象 | ax-task 对象 | 等待与交接 | 持有期间 |
| --- | --- | --- | --- |
| `raw_spinlock_t` | `RawSpinLock`、`RawSpinRwLock` | 原子自旋；不做 PI | 普通方法关闭抢占；IRQ-save 方法同时关闭 IRQ；unpinned 方法由 unsafe 调用方提供保护 |
| `spinlock_t` | `SpinLock` | PI gate，保存外层状态的 RT 锁等待 | 可抢占，禁止迁移及普通睡眠；IRQ-save 拼写不改变硬件 IRQ |
| `rwlock_t` | `SpinRwLock` | PI 写者 gate，等已有读者排空 | 同 RT spinlock；不能向多个已有读者捐赠优先级 |
| `local_lock_t` | `LocalLock` | 先 pin，再选择每 CPU 锁，再取得 RT spinlock | 同 CPU 任务可抢占竞争；解锁后释放外层 pin |
| `mutex` | `Mutex`、`InterruptibleMutexExt` | 普通 PI 等待，取消谓词和 handoff 竞争 | 普通可睡眠任务上下文，不自动 pin |
| `rw_semaphore` | `RwSemaphore` | 与 RT rwlock 共用写者 gate 和 reader-drain，使用普通等待 | 不自动 pin；已有读者完成后才允许独占 |
| `semaphore` | `Semaphore` | raw IRQ-safe FIFO，直接交付许可，不做 PI | `up/try_down` 可在 IRQ 调用；阻塞、超时和中断等待只在任务上下文 |

Linux 对照包括 `kernel/locking/spinlock_rt.c`、`rwbase_rt.c`、`rtmutex.c`、`semaphore.c`、`include/linux/local_lock_internal.h` 和 `kernel/sched/core.c`。`SemaphoreRegistration` 在同一 raw 锁下裁定取消与授予；队列扩容在任务上下文分配，IRQ 端不销毁等待者对象。

### 1.5 迁移禁止

`ThreadAffinityState` 区分 `requested_affinity` 与有效 `affinity`。首次 `MigrationGuard` 在登记表、任务调度锁和 owner rq 事务下安装预建的单 CPU 掩码，并刷新所有调度类的迁移候选索引；嵌套 guard 只增加深度。外层释放恢复最新请求，并投递 owner reconciliation。CPU 下线在同一登记表锁下拒绝仍有 pin 的 CPU。

锁获取后建立 RT 临界区和迁移保护；释放按 `migrate_enable → 结束 RT 临界区 → PI unlock` 排列。普通管理引用不是迁移保护。Starry 的远端 `sys_sched_setaffinity` 使用 `set_affinity_and_wait`，异步请求只有在目标 CPU 满足新 mask 后才完成。

## 2. 发布与回收

创建令牌拥有取消责任；登记表拥有已转交资源。逻辑退出通知不代表线程已经物理切出，也不代表最终引用已经消失。

### 2.1 创建事务

`PreparedThread` 保存未运行线程，`StagedThread` 表示完成可失败准入的发布事务。首次激活前不得运行 trampoline 等待用户态发布，取消也不需要调度新线程。队列节点在构造阶段预留；CPU 下线、亲和性和远程投递必须与最终激活串行。

`TaskSystem::stage_new_thread` 在登记表和线程调度锁下选择目标 CPU，把 `PreparedMigrationDelivery` 保存在 `ThreadRecord::activation`。该令牌持有 CPU 接收端和 inbox 投递租约；CPU 下线必须等待已有租约退出。亲和性修改先预留新目标，再替换旧令牌，失败不提交新亲和性。策略修改仍由原调度所有者处理，激活前刷新预留投递的负载信息。

`StagedThread::activate` 在 IRQ/抢占保护下消费预留，只执行一次 `New` 到可运行状态的转换。身份发布后的正常路径没有可恢复错误；运行时未安装等违反契约的情况仍作为不变量失败。低层 `TaskSystem::start_thread` 复用同一准入协议，并拒绝绕过持有公共执行对象的创建令牌。

`PreparedThread/StagedThread::drop` 只将预分配的 `ThreadExecution::cancellation_node` 和一个强引用投递给现有 task-work inbox，不在硬中断或原子上下文取得登记表锁。reaper 的 `process_thread_cancellation` 先取走并在锁外释放预留，再执行私有 `mark_unqueued_exited`，把未调用的入口闭包交给任务上下文退出回调释放。并发调度控制仍忙时保留请求重试。公共 `mark_exited` 与 `start_thread` 都拒绝 managed 线程，避免绕过创建令牌的唯一所有权。`ThreadExecution::finish` 在闭包析构结束后通知等待者；创建令牌不需要启动线程来完成清理。

### 2.2 回收事务

切换尾部释放 CPU 所有权后，任务上下文回收执行资源。OS 扩展由强句柄和在途回调保护，最终析构不能提前。active-MM 令牌继续由独立的最后 CPU 回收协议处理；同 MM 的 user/kernel 转换不能跳过用户返回屏障。

`TaskSystemState::take_exited_execution` 要求线程已经 `Exited` 且不在 CPU 上，没有迁移、deadline 预留或通知、在途回调、inbox 投递及睡眠定时器。它从登记记录中唯一取出 `ThreadResources`，清除调度状态中的上下文和 MM 句柄，并临时持有强管理租约，防止并发 join 先释放 OS 扩展。任务上下文 reaper 在锁外依次释放架构上下文、TLS 和栈，把 MM token 交给独立回收协议；完成后以 release 发布 `execution_reclaimed`。

这个租约只覆盖一次资源释放事务。普通 `ThreadHandle` 和 `ThreadExtensionBorrow` 保留的是任务对象与 OS 状态，不能通过它们访问已释放的执行上下文或栈。当前调用链没有远端栈读取 API，因此不引入没有消费者的栈引用类型；将来增加栈观察者时必须先加入独立栈保护，不能使用管理句柄代替。

最终对象回收继续经过登记表锁、外部租约、generation 检查、调度活动关闭和回调 claim 协议。CPU 当前对象的裸指针借用由 IRQ/抢占 pin 与 on-CPU 所有权保护，inbox 裸指针由投递租约保护。这里的等价性依赖这些读者逐一退出，并非仅凭 `Arc::strong_count` 推断 Linux RCU 宽限期已经结束。敏感上下文丢弃最后一个管理引用只发布 reaper 工作，不直接调用可能取得睡眠锁的 OS 析构。

取消队列的强引用保护节点所在的执行对象；`SchedulerInbox` 的 epoch grace 完成后才拆下节点并把引用交给消费者。`New` 登记记录和唯一创建令牌阻止提前回收；这条读者协议与强引用各负其责。`validate_task_work_context` 在直接 reap 和 task-work 消费者取得任何锁之前拒绝硬中断、IRQ/抢占保护、未结束的调度交接及 RT 锁临界区，防止睡眠析构绕过普通 join 的上下文检查。

### 2.3 完成交接

`ThreadExecution` 是唯一的公共入口、返回码和完成等待所有者。正常返回与显式 `exit_current` 都先发布逻辑完成，再提交退出调度；`wait` 只承诺返回码可见。`join` 遇到仍在切出、仍有回调或其他租约的线程，把最终回收交给 reaper，不等待无关引用归零。

公共 `thread_entry` 首先调用 `finish_initial_context_switch`，再取走入口闭包；进入可能永不返回的闭包前释放临时 core 引用。普通恢复则沿原调度栈完成尾部。OS `on_switch_in/out` 继续在交接临界区维护 CPU 当前身份，`on_exit` 和 `drop` 在任务上下文执行，不能把三个阶段合并成同一个 hook。

### 2.4 地址空间

`RuntimeSwitchPlan` 仍把两个架构上下文和各自的逻辑 MM 作为一次不可重放的交接提交。`prepare_runtime_address_space_switch` 根据逻辑 MM 身份、CPU 的 active-MM 租约和架构要求准备 `PreparedAddressSpaceSwitch`，架构寄存器切换前执行无失败的 commit。此次生命周期重构不重排这条协议。

| 转换 | 地址空间所有者与动作 | 回收边界 |
| --- | --- | --- |
| user → user | 切到 next 的 MM；共享 `AddressSpaceCpuState` 身份可以走同 MM 分支 | 旧 active-MM 租约随实际硬件切换转交 |
| user → kernel | 内核线程没有自己的 user MM，进入 `KernelLazy` | 内核任务对象的 MM token 与 CPU 借用的 active-MM 分开 |
| kernel → user | 安装 next 的 MM；AArch64 恢复 lazy 路径替换的 TTBR0 | 仅在不再有 CPU 激活租约时释放旧 MM |
| kernel → kernel | 继续 CPU 的 lazy-MM 状态 | 不因释放一个内核线程的栈而释放 CPU 仍借用的 MM |

同 MM 快路径不代替独立的 membarrier 注册、IPI 同步和用户返回检查。`UserExecutionContext` 的创建和 exec 后刷新继续验证调度选择的 MM、硬件根和 CPU footprint；硬件返回边界继续保留原 TLB/同步要求。普通 QEMU clone/exec 的通过只能证明运行路径，不能替代针对弱内存排序的独立屏障测试。

## 3. 迁移验证

本节只记录实际取得的证据；未执行的矩阵不记为通过。重构保留分层资源语义，不用删断言或延长超时掩盖错误。

### 3.1 回归

在现有 ArceOS `task-wait-queue` 用例中增加 staged 线程必须保持 `New` 的断言，并通过公开 wake 验证不能提前激活。先在旧实现执行，再用同一断言验证修复。

两项调度回归分别取得同一断言的失败和通过：旧 stage 返回 `Running` 而非 `New`；旧回收只有在管理句柄销毁后才释放执行资源。修复后保留强句柄也能观察 `execution_reclaimed`，并继续安全借用直接 OS 扩展。`wait_queue/lifecycle.rs` 还检查 prepare/stage 取消不执行入口、闭包只析构一次、退出码 17、自等待拒绝以及 switch/exit/drop 的上下文。

`test-clone-fp-state` 原先在非 RISC-V 上直接跳过；现在四架构都使用原始 clone，并在子进程第一段代码中读取预先写入的 FP 寄存器。新增检查在 AArch64、LoongArch 上都得到“三项通过、一项寄存器断言失败”。根因是这些架构只复制了通用陷阱帧，未把父线程的 FP 状态装入新的调度上下文。

`UserContextOptions::inherit_current_fp` 把非 RISC-V 子线程继承集中到 `ax-runtime` 的未发布资源装配阶段。x86 沿用 lazy xstate 所有者处理；AArch64 与 LoongArch 的 `TaskContext::clone_user_fp_state_into` 在 CPU pin 下保存 eager FP 状态；RISC-V 保留显式快照和 FS 状态。该范围覆盖当前支持的标量 FP/现有 SIMD 保存格式，不宣称新增 SVE、SME、LSX 或 LASX 支持。Linux 对照为固定提交的 [arm64 arch_dup_task_struct](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/arch/arm64/kernel/process.c) 与 [LoongArch arch_dup_task_struct](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/arch/loongarch/kernel/process.c)。

### 3.2 完成条件

迁移工作区调用方并删除旧接口，验证项目 fmt、clippy、适用 std 测试及 ArceOS/Starry 四架构和适用 Axvisor QEMU。真实调度不使用宿主伪 runtime 替代。性能仅引用相同环境实际运行的基准；已执行结果与限制分别记录，不能把核心重构的完成当作所有 RT 验收项均已完成。

### 3.3 既有验证限制

RISC-V `task-scheduler-irq-window` 在当前代码和独立基线 `4c757cc0e1fcda825c1a4cedbcf9d187c11485c8` 上均失败于 `CPU0 workers did not all become runnable`。保留原断言、工作线程数和超时，不计为通过。独立 `task-irq` 与四架构 `task-wait-queue` 的通过不能替代这个压力用例。

`ax-task` 的整个 host-test 库测试在当前实现和独立基线上均因缺失 runtime trait-ffi 链接符号无法生成测试程序。当前只运行可独立链接的现有纯算法 integration tests；没有添加假的运行时实现来掩盖链接缺口，也没有把未执行的库内 Loom 算作通过。

第一次完整 clippy 在共享 `target/.future-incompat-report.json` 中遇到来自并行构建的 `bitmaps` 报告。检查器的 AArch64 例外仅允许指定的 `core/memchr` Rust #134375 报告。验证因此转到独立检出，保留门禁和允许范围。

### 3.4 系统验证记录

验证以 `4c757cc0e1fcda825c1a4cedbcf9d187c11485c8` 为修改前基线。公共生命周期迁移提交为 `58d87bb85d`，AArch64/LoongArch FP 修复单独提交为 `9f65217fae`。本地日志使用 `/tmp/ax-task-*.log` 前缀；四架构均通过对应 QEMU 的真实执行，不把目标编译等同于运行。

| 验证入口 | 已执行范围 | 结果与证据 |
| --- | --- | --- |
| `cargo fmt -- --check` 与 Starry include 文件的定向 rustfmt | 修改的 Rust 源码；显式覆盖 Cargo fmt 不遍历的 `include!("root.rs")` 子模块 | 通过 |
| `cargo xtask clippy --package ...` | ax-task、ax-runtime、ax-net、ax-api、ax-posix-api、ax-std、starry-kernel、axvm、arceos-test-suit、arceos-scheduler-latency-bench、ax-cpu | 隔离检出前 199 项通过，修复测试遗漏消费 affinity token 的警告后剩余三个包 73 项通过；去重后 271 个组合全部通过。`ax-task-clippy-isolated.log`、`ax-task-clippy-remaining.log`；最终 Rust 源码逐文件一致 |
| `cargo xtask arceos test qemu --test-group rust --test-case all --arch <arch>` | riscv64、x86_64、aarch64、loongarch64 | 四架构通过，`ax-task-arceos-all-<arch>.log` |
| `cargo xtask arceos test qemu --test-group rust --test-case task-wait-queue --arch <arch>` | 发布、提前 wake、取消、退出码、保留管理引用时的资源回收、直接扩展 hook | 四架构最终复跑通过，包含 IRQ 保护中最后引用释放与创建失败探针；`ax-task-irq-drop-<arch>.log` |
| `cargo xtask arceos test qemu --test-group rust --test-case task-irq --arch riscv64` | 首次入口与 IRQ/抢占交接 | 通过，`ax-task-final-riscv64-task-irq.log` |
| `cargo xtask arceos test qemu --test-group c --test-case pthread-basic --arch riscv64` | pthread 创建、TID 发布、等待与退出 | 通过，`ax-task-pthread-c.log` |
| `cargo xtask starry test qemu --arch <arch> -c qemu/<case>` | 四架构各运行 `test-clone-fp-state`、`syscall-test-clone-tls`、`test-execve`、`test-thread-lifecycle-exec`、`test-wait-clone-options`、`syscall-test-vfork`、`bugfix-bug-sigchld-si-code-exit-group` | 28 个架构/用例组合通过；FP 修复后的 RISC-V/x86 最终复跑也通过，`ax-task-fp-final-<arch>.log` |
| `cargo xtask axvisor test qemu --arch aarch64 --test-group normal --test-case gicv3-timer-stress` | vCPU 统一 stage/activate 与 GICv3 ITS timer SMP 场景 | 通过，`ax-task-axvisor-aarch64.log`，包含 `AXVISOR_GICV3_ITS_TIMER_STRESS_PASSED` |
| `cargo xtask test --since 4c757cc0e1` | 生命周期迁移后的标准库候选 | 首次 13 个软件包通过，`ax-task-std-tests-final.log`；FP 提交后 `--since 58d87bb85d` 选出含 ax-cpu 的 12 个软件包，也全部通过，`ax-task-std-fp-final.log` |
| ax-task 现有独立纯算法 integration tests | `fair_nohz_state`、`fair_hrtick`、`rt_priority_order`、`pi_mutex_entry` | 13 项通过，`ax-task-isolated-models.log`；不含未能链接的整个库内测试 |

纯算法 integration tests 不在项目 std 允许列表中，也没有按 integration target 选择的 xtask 入口，因此在核对入口实现后使用原生 `cargo test -p ax-task --test <target>`。这个例外只运行不依赖 fake runtime 的已有算法测试，不用于 ArceOS、Starry 或 Axvisor 系统行为。

### 3.5 生命周期开销

在既有 `arceos-scheduler-latency-bench` 中保留无切换 yield 和双 FIFO 线程切换测量，增加 `spawn` 调用耗时及 WaitQueue 通知到线程恢复耗时。基线检出只应用同一测量代码和同一有效 UEFI 配置，生产实现仍为 `4c757cc0e1`。原 x86 配置把动态 PIE 直接交给 `-kernel`，报 `Error loading uncompressed kernel without PVH ELF Note`；改为与 Rust 套件一致的 `uefi = true/to_bin = true` 后两侧正常运行。

两侧均执行 `cargo xtask arceos qemu --package arceos-scheduler-latency-bench --arch x86_64 --smp 1 --qemu-config apps/arceos/scheduler-latency-bench/qemu-x86_64.toml`，使用 q35、CPU max、256 MiB、单 CPU、TCG。每侧启动三次，交错顺序为基线、新、新、基线、基线、新；每次运行七轮，对七轮“平均每次操作耗时”取中位数，再对三次启动的结果取中位数。日志为 `ax-task-bench-lifecycle-{baseline,current}-{1,2,3}.log`。

| 指标 | 基线 ns | 重构后 ns | 变化 | 三次启动的中位数范围：基线 / 重构后 ns |
| --- | --- | --- | --- | --- |
| spawn 调用 | 254757 | 263829 | +3.56% | 220098–266711 / 260311–271068 |
| WaitQueue 唤醒到恢复 | 35950 | 36340 | +1.08% | 34137–38454 / 35369–41305 |
| 无切换 yield | 1017 | 1011 | -0.59% | 960–1018 / 980–1073 |
| 双 FIFO 线程切换 | 2877 | 2707 | -5.91% | 2796–3001 / 2557–2996 |

spawn 每轮先预热 16 次，再测 100 次；join 放在计时区间外，但高优先级子线程的首次激活/抢占属于 spawn 的可见成本。wake 每轮先预热 100 次，再测 2000 次；同 CPU 的 FIFO waiter 优先于 fair 协调线程，generation 确认阻止通知合并，计时包含通知发布、入队、切换和条件重检。原 yield/switch 的 2000 次预热、20000 次测量保持不变。

这些范围相互重叠，只能说明本次 TCG 微基准中没有数量级退化，不能据此保证性能提升或实时延迟上界。宿主同时有其他构建负载，数据也不是 Linux PREEMPT_RT 运行基线；本地 Linux 的非 RT 配置没有参与计时。

### 3.6 交付边界

本地交付包括接口迁移、首次激活、分阶段回收、FP 继承修复、系统回归和微基准。公共执行接口与首次激活/回收协议在一个迁移提交中一起收敛，未完全做到计划要求的接口提交与语义提交分离；后续 FP 修复单独提交。上述生命周期阶段先以本地提交交付；后续 RT 锁工作按用户要求转为 PR 和 CI 验证。

完整 RT 验收仍有明确缺口：第 3.3 节的既有 IRQ 压力失败与整个 host/Loom 库测试链接问题，以及没有直接系统级证据的 syscall 表项。初始交付尚未逐阶段注入栈/TLS/架构上下文分配失败，后续第 5.4 节已补齐真实运行时的阶段故障与回滚次序验证。第 5.8–5.10 节进一步补齐四架构实际调度器下线/上线、activation 预留与用户态 membarrier 证据；物理 CPU 停机/重新引导和穷尽弱内存交错仍不在这些结果的证明范围内。合入前仍需要计划要求的调度及 unsafe 领域审查。

## 4. Linux 可观察行为

兼容性结论限定为本次改动经过的创建、FP/TLS 初始化、发布和退出调用链，不表示已经审计每个系统调用的所有 flag。编号中 X 表示 x86_64，G 表示 AArch64、RISC-V64、LoongArch64 共用的 asm-generic 编号；没有独立 fork/vfork 系统调用的目标由 libc 使用 clone。

### 4.1 创建与发布

`sys_clone`、`sys_clone3`、x86 `sys_fork/sys_vfork` 最终进入 `CloneArgs::do_clone_in_cgroup`。参数、用户内存和权限检查仍在原入口完成；调度资源先 prepare/stage，再提交 cgroup、PID 和进程拓扑，最后 activate。扩展扁平化没有合并 Linux PID 身份与调度器 generation-bearing `ThreadId`。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| clone：FP / X56、G220 | [v7.1 固定提交 fork.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/fork.c) | 子线程首次返回时继承父线程已有 FP 状态 | `sys_clone → do_clone_in_cgroup → UserContextOptions → TaskContext`，每线程 FP | 正确 | 四架构真实寄存器断言；AArch64/LoongArch 同一断言红绿 |
| clone：TLS/共享 MM / X56、G220 | [v7.1 固定提交 fork.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/fork.c) | pthread 子线程看到自己的 TLS，并共享指定进程资源 | `sys_clone → Thread → PreparedUserTask → stage → 身份发布 → activate` | 正确 | 四架构 `syscall-test-clone-tls`、非主线程 exec |
| clone3 / 四架构435 | [v7.1 固定提交 fork.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/fork.c) | 检查结构长度与参数后创建，失败不发布子进程 | `sys_clone3 → CloneArgs::try_from → do_clone_in_cgroup` | 无法确认 | 共用创建主体已验证；本次未执行 clone3 专项系统测试 |
| fork / X57；G 无独立入口 | [v7.1 固定提交 fork.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/fork.c) | 创建独立子进程，父子返回不同值 | `sys_fork → sys_clone(SIGCHLD)` | 无法确认 | libc fork 场景通过，但不据此断言直接 X57 已执行 |
| vfork / X58；G 使用 clone | [v7.1 固定提交 fork.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/fork.c) | 父线程等待子进程 exec 或退出 | `sys_vfork → sys_clone(VFORK|VM) → ProcessData::notify_vfork_done` | 正确 | 四架构 `syscall-test-vfork`；G 验证的是 clone 入口的 VFORK 语义 |

`clone3` 与直接 `fork` 的专项证据缺口单独列出，不能由共同辅助函数或 libc 调用名称推断已经覆盖对应入口。

### 4.2 替换与退出

`Thread` 直接内嵌后，exec 的 de-thread、MM 替换、退出时资源释放、zombie 发布和父进程等待仍由 Starry 的进程/线程所有者完成。公共 `ThreadHandle::wait` 不替代 Linux 的 wait 系统调用，也不把物理栈释放当作 zombie 可见性的条件。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| execve / X59、G221 | [v7.1 固定提交 exec.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/exec.c) | 替换 MM；非主线程 exec 保持进程 PID 并成为主线程 | `sys_execve → do_execve` 共享执行路径、`UserTaskRef::switch_address_space`、进程 PID 拓扑 | 正确 | `test-execve` 与 `test-thread-lifecycle-exec`；仅涵盖有效 argv/envp 与用例中的错误路径 |
| execveat / X322、G281 | [v7.1 固定提交 exec.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/exec.c) | 解析 fd/相对路径和 flags 后执行同一 MM 替换事务 | `sys_execveat → 路径/FD 校验 → do_execve` 共享路径 | 正确 | `test-execve` 的 raw execveat 成功与错误分支 |
| exit / X60、G93 | [v7.1 固定提交 exit.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/exit.c) | 退出当前线程，clear-child-tid 唤醒 joiner；最后线程触发进程退出 | `sys_exit → do_exit(false) → Thread/ProcessData` | 正确 | pthread TLS、线程退出及非主线程 exec 用例 |
| exit_group / X231、G94 | [v7.1 固定提交 exit.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/exit.c) | 退出线程组；先发布终态，再通知父进程 | `sys_exit_group → do_exit(true) → publish_zombie → SIGCHLD/child_exit_event` | 正确 | `bugfix-bug-sigchld-si-code-exit-group` |
| wait4 / X61、G260 | [v7.1 固定提交 exit.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/exit.c) | 按子进程与 clone 选项等待并取得退出状态 | 分派至 `sys_waitpid → WaitCandidateScan → 进程身份/zombie → wait_on_pollset` | 正确 | `test-wait-clone-options`、clone FP、exec 和 vfork 的父进程 wait |
| waitid / X247、G95 | [v7.1 固定提交 exit.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/exit.c) | 按 selector 和 options 返回 siginfo，支持独立等待规则 | `sys_waitid → WaitIdSelector → WaitCandidateScan → 进程身份/zombie` | 无法确认 | 本次未执行 waitid 专项系统测试 |

原 `test-mt-execve` 是因其他 exec 参数兼容问题隔离的 known-fail 用例，本次没有把它的未执行状态记成通过。新增用例使用有效的 argv/envp 单独验证本次生命周期改动，不取消原隔离记录。

### 4.3 发布期间调度修改

线程身份可在 activate 前被其他线程看到，因此 stage 期间的策略和亲和性修改必须保持可用。ArceOS 回归直接覆盖公共调度 API 对 `New` 线程的更新；Linux syscall 层仍保留各自的用户内存、权限和参数校验。没有直接运行 syscall 专项用例的入口不标为完全兼容。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| sched_setaffinity / X203、G122 | [v7.1 固定提交 syscalls.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/sched/syscalls.c) | 权限和 cpumask 校验后提交亲和性；已发布新线程不能因旧目标预留而失效 | `sys_sched_setaffinity → 调度 handle → TaskSystem::request_thread_affinity → ThreadRecord::activation` | 无法确认 | stage 期间公共 API 更新的 QEMU 已通过；缺 syscall 级竞争用例 |
| sched_setscheduler / X144、G119 | [v7.1 固定提交 syscalls.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/sched/syscalls.c) | 校验策略与权限后更新调度实体 | `sys_sched_setscheduler → apply_scheduler_update → TaskSystem`，每线程调度状态 | 无法确认 | 公共 API staged policy 更新已测；未执行该 syscall 专项 |
| sched_setparam / X142、G118 | [v7.1 固定提交 syscalls.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/sched/syscalls.c) | 保留策略并更新优先级 | `sys_sched_setparam → 读取当前策略 → apply_scheduler_update` | 无法确认 | 未执行该 syscall 专项 |
| sched_setattr / X314、G274 | [v7.1 固定提交 syscalls.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/sched/syscalls.c) | 检查 sched_attr、权限和准入后提交策略 | `sys_sched_setattr → sched_attr 解析 → apply_scheduler_update` | 无法确认 | 未执行该 syscall 专项 |


## 5. RT 锁验证

本节记录 `a22f81c016` 之后的锁语义改动，不能把第 3 节此前的通过记录当作这些新改动的验证。

### 5.1 确定性交错

`task/pi_mutex/rt_locks.rs` 通过公开运行时 API 检查 RT 锁持有期间的高优先级抢占、RT 锁内层阻塞期间的外层超时、读者排空、同 CPU local lock 竞争、嵌套迁移守卫与远端亲和性更新，以及 semaphore 的 FIFO 许可、超时和取消。外层超时测试先用高优先级任务抢占 owner，明确结束 owner spinning，再触发内层 park；失败时用普通 wake 取回测试控制权，并检查实际 park disposition，不依赖 QEMU 超时判定。

旧 SpinLock 在同一断言失败于 `Linux RT spinlock must allow higher-priority preemption while held`；新实现通过。初版内层 park 覆盖外层 timeout 代次时，同一断言失败于 `outer timeout must survive the RT-lock inner park`；三条超时路径改用普通等待代次后通过。日志分别为 `/tmp/ax-task-rt-spin-preemption-{red,green}.log` 和 `/tmp/ax-task-rt-outer-timeout-{red,green}.log`。

### 5.2 验证范围

RT 锁与调用方静态检查覆盖 9 个软件包、215 个组合，全部通过；idle pin 检查前移后，ax-task 的 6 个组合又独立通过。`cargo fmt -- --check` 通过，`cargo xtask test --since a22f81c016` 选出的 13 个软件包全部通过。`lifecycle_state` 独立编译实际生命周期实现，13 项状态与 Loom 用例通过，没有增加 fake runtime。

最终四架构各执行 ArceOS `all` 和 `task-pi-mutex`，8 个运行全部通过，包含外层超时、semaphore 定时器注册失败回滚及真实硬中断 `up`。Starry 最终复跑已完成 RISC-V 的 `test-clone-fp-state`、`syscall-test-clone-tls`、`test-execve`、`test-thread-lifecycle-exec`。随后按用户要求停止扩大本地矩阵，改由 PR CI 验证；未完成的其余 Starry/Axvisor 运行不记为通过。

定时器容量故障通过仅测试用的 `fault-injection` feature 注入到当前线程下一次 semaphore 定时器注册。遗漏 park 取消时，QEMU 触发 `0x5041_0003` invariant；修复后返回 `TimerCapacity` 且线程保持 `Running`。IRQ `up` 不克隆或销毁 wake handle：生产者保持抢占保护，在 raw 锁内发布 handoff 完成并释放队列引用，等待者观察完成后才能销毁自身登记。

本节初次验证仅覆盖 idle 提前返回的下线 pin 检查；后续第 5.8 节已通过实际 idle owner 验证调度器下线/上线。当前仍没有平台物理停机、IRQ/IPI 撤销和重新引导的完整热插拔契约。Linux 隐含 RCU 读侧的对象保护采用第 2.2 节逐对象租约与锁借用；不宣称新增通用 RCU API。本节最初验证阶段只测量了独立 `a22f81c016` 检出。后续按 #2308 完成的 dev/PR/dev 对照已确认回退；按当前要求暂停性能处理，不使用第 3.5 节旧结果推断当前分支无回退。

### 5.3 静态审查修复

`wait_queue/lifecycle_review.rs` 对三项问题分别取得 x86_64 四核 QEMU 确定性失败：原子上下文取消同步变为 `Exited`、公共 `mark_exited` 成功消费 managed 退出权、直接回收未优先返回 `UnsafeContext`。同一断言在修复后通过，日志为 `/tmp/pr2357-review-{cancel,owner,reclaim}-red.log` 和 `/tmp/pr2357-review-green.log`。取消测试让真实 reaper 的退出回调等待协调线程，保证观察时消费者尚未处理取消请求，不依赖竞态概率。

补充用例在真实硬定时器回调中取消 prepare/stage 令牌，包含不保留管理句柄的最后引用路径；同时检查 raw 锁、RT 锁及 IRQ 保护中的直接回收均被拒绝。最终四架构 `task-wait-queue` 通过，x86_64 `task-pi-mutex` 通过；定向 `cargo xtask clippy --package ax-task --package arceos-test-suit` 的 2 包、46 组合通过。全工作区 clippy 交由 PR CI。日志为 `/tmp/pr2357-review-final-{x86_64,riscv64,aarch64}.log`、`/tmp/pr2357-review-green-loongarch64.log`、`/tmp/pr2357-review-pi-green.log` 和 `/tmp/pr2357-review-clippy.log`。


### 5.4 资源装配收尾

用户上下文装配删除了 `InitialContextState`、`ThreadResourceBackend`、`ThreadResourceCreationFailure` 和 `UnreleasedThreadResources`。`UserContextOptions` 直接拥有必需的 MM 和架构 FP 配置，不再保留未被调用的 kernel 分支。`create_user_resources` 在第一次分配前建立 `UnpublishedContext`，成功时转交完整 `ThreadResources`，失败时由同一任务上下文释放 context、TLS、stack。MM 在完成架构初始化前仍由配置持有；因此任何失败都不能遗失它的唯一所有权。执行资源释放遵循当前 TaskRuntime 的一次性消费契约，不恢复旧的 Busy/重试状态。虚拟栈 shootdown 失败时的隔离区由 ax-mm 独立持有，不能重试已经消费的栈句柄。

`ax-runtime/fault-injection` 只用于真实内核测试。`ThreadCreationProbe` 用创建者 ThreadId 限定一次故障，只记录实际提供者的分配入口和释放顺序；其他任务及硬中断不受影响。删除的宿主 fake backend 测试由 ArceOS `task-wait-queue` 的栈/TLS/context/bind 失败回滚和 Starry `user_context_resource_failure_rollback` 的 MM/栈/TLS/context/FP/bind 回滚替代。FP 注入验证的是上下文已创建、FP 初始化尚未结束时的事务回滚，不声称默认 FP 初始化新增了可恢复硬件错误。

### 5.5 MM 交接验收

Starry `thread_lifecycle_axtest::user_kernel_mm_switch_matrix` 让同 CPU、同优先级 FIFO 的两个带 MM 上下文和两个 kernel 上下文依次运行，验证 UU、UK、KK、KU 四类实际切换。它们使用始终有效的内核页表根且不进入用户态；运行时 MM 身份和 CPU lease 是真实对象，带 MM 的入口检查实际硬件根。首次入口、阻塞恢复和 yield 后检查 IRQ 已恢复、阻塞许可有效。

同一用例验证 active-MM 独立寿命：第一个 MM 在被第二个 MM 替换后释放；最后一个 user 线程退出并 join 后，其 MM 仍由 kernel lazy-MM 借用；再切换到另一 MM 后才释放一次。没有用管理句柄引用数代替 CPU lease 退出证明。用户返回前的 membarrier/TLB 屏障保留原实现；该用例验证 MM 交接与回收，不把 kernel 闭包执行误记为返回用户态或弱内存屏障的系统证明。

用户上下文测试使用现有 `cargo xtask ktest qemu -p starry-kernel --test axtest_kernel --features axtest,smp --arch <arch>`；`.github/ci/checks/starry.toml` 的四架构任务已经调用该入口，不需要额外的 CI 特例。ArceOS 的真实 std 套件与 uspace 的 TLS 寄存器所有权不同，不能为了复用一个测试二进制而同时启用两种模式。


### 5.6 完成 ID 的测试契约

最终 AArch64 kernel axtest 在既有 `block_runtime_async_double_read` 中触发 `left: 1 / right: 1`。`nvme-driver::NvmeQueueState::complete_one` 在终结请求时把 CID 放回空闲池，因此已完成对象存活期间出现同 CID 是合法行为，与任务调度错误不同。`rdif-block::RequestId` 明确这项队列局部、可复用契约。

该用例保留两条真实异步读取、完成状态和长度检查，并把已交付的结果信封固定为合法的相同 ID，使回归不依赖硬件完成时序。原“不相等”断言确定性失败，记录为 `/tmp/pr2357-cid-contract-red.log`。修复验证两个已完成 DMA 对象的独立所有权：改写第一个 CPU 缓冲区后，第二个缓冲区内容必须保持不变。改写只发生在完成后的内存，不提交设备写入，也不通过放宽超时或重试掩盖失败。


### 5.7 收尾验证记录

本节记录资源事务提交 `d9f5be6997` 及完成 ID 契约修复 `d467030dcb` 的验证；后续缺口补充见第 5.8–5.10 节。完整工作区 clippy 按要求交给 CI，本地只执行受影响包检查。性能回退按当前要求暂停，未混入任何性能实验。

| 检查 | 本轮证据 |
| --- | --- |
| `cargo fmt -- --check`；Starry include 子模块定向 rustfmt | 通过；`block_runtime_axtest.rs` 与 `thread_lifecycle_axtest.rs` 显式覆盖 |
| `cargo xtask clippy --package ax-runtime` | 26 组合通过；`/tmp/pr2357-resource-runtime-clippy.log` |
| `cargo xtask test --since 7e62c2d9e1` | 11 个包全部通过；`/tmp/pr2357-plan-std.log` |
| ArceOS `task-wait-queue` | 四架构真实资源故障回滚、取消、回收通过；`/tmp/pr2357-resource-final-<arch>.log` |
| Starry kernel axtest | 最终四架构全部通过；AArch64 177 项，其余各 176 项；`/tmp/pr2357-closeout-final-<arch>.log` |
| 完成 ID 契约 | AArch64 原断言确定性失败；同一合法重复 ID 输入修复后通过；`/tmp/pr2357-cid-contract-red.log` |

这些结果覆盖本轮实现和直接调用链；CI 仍按最新推送执行，未等待或宣称其成功。后续调度器 CPU 周期及用户态 MM 屏障的证据见第 5.8–5.10 节；合入前的领域审查要求不由本地测试替代。


### 5.8 CPU 生命周期与锁顺序

`probe_idle_cpu_round_trip` 仅在 `fault-injection` 下可用，由实际 `run_idle` 在普通调度工作排空后调用。整个 `take_cpu_offline → bring_cpu_online` 保持同一个 pinned owner 借用及 IRQ 排除，不让调度循环观察到 offline 的本地 owner。它验证调度器和运行时 hook 的真实事务，不执行物理断电、平台 IRQ/IPI 撤销或 AP 重新引导。

`request_idle_cpu_round_trip` 先发布不可消费的 `ARMING`，投递真实 scheduler work；生产者租约退出后发布目标 CPU，再发物理通知，覆盖 idle 先观察到 `ARMING` 的情况。完成结果与消费状态放在同一个原子字中，避免旧消费者清除下一次请求。`idle_cpu_reservation_round_trip` 验证 staged 预留拒绝下线，取消或激活并退出后允许下线/上线，并用目标 CPU 上的实际睡眠验证恢复后的调度和 clockevent。

新增回归发现 `prepare_cpu_offline` 曾在 IRQ 关闭、调度登记表与 root-domain 锁内调用全局 `kernel_aspace().lock()`，并可能等待远端 TLB shootdown。该调用违反 hook 的不可阻塞契约，会与持 MM 锁并等待目标 CPU 响应的路径形成锁反转。删除这里的 `retry_kernel_tlb_reclaims` 包装和调用；隔离资源继续归 ax-mm 的 TLB quarantine 持有，普通 MM map/unmap/protect 等事务通过 `retry_tlb_quarantine` 重试，未提前释放资源。active-MM 退出和本地 clockevent 停止仍保持原事务顺序。

`offline_does_not_lock_global_mm` 在 CPU2 持有真实全局 MM 锁，CPU0 请求 CPU1 下线/上线，必须在释放 MM 锁前收到成功回复。旧实现确定性失败于 `CPU offline must not acquire the global kernel MM lock`，修复后同一用例通过；日志为 `/tmp/pr2357-offline-mm-lock-{red,green}.log`。失败路径先释放持锁线程并收回复，避免留下 IRQ-off 自旋 CPU。

Starry 的 `migration/N` stopper 是固定 CPU 的 `KernelStop` 线程，尚无 hotplug park 契约，不能为测试放宽下线门禁。`idle_cpu_cycle_rejection_retains_active_mm` 验证这种拒绝不会释放 idle 的 active-MM；随后实际切换到新 MM，旧 owner 才释放一次。

### 5.9 用户返回与屏障证据

`syscall-test-mm-lifecycle/src/membarrier.c` 在现有真实用户态测试中加入 512 轮 store/load litmus。两个 pthread 绑定不同 CPU，注册并调用 `PRIVATE_EXPEDITED`；远端只使用编译器屏障，隔轮经过 nanosleep 和实际用户返回。pthread barrier 仅用于轮次边界，测试中的 store/load 之间没有额外 acquire/release 握手。双侧同时读到旧值是失败，成功标记为 `MM_MEMBARRIER_USER_RETURN_PASSED`。

源码证明与运行证据分别维护：`scheduled_membarrier_state` 在 kernel thread 上保留借用 MM 的身份；`collect_membarrier_targets` 在 CPU publication 租约和 raw rq 锁保护下选择同 MM 的 CPU。因此同 MM 的 user→kernel→user 不采用 Linux NULL-mm 分支的跳过 IPI 路径。`sync/membarrier/operations.rs::membarrier` 在扫描前与最后同步应答后执行 SeqCst fence，`synchronize_membarrier_cpu` 的硬 IPI 回调执行全屏障；rq 身份改变也有 SeqCst fence。`SameUser` 快路径不能绕过上述同步，也没有用额外的无依据 fence 掩盖所有权问题。

四架构 QEMU 验证真实 syscall、用户执行和返回路径，但不穷尽硬件弱内存交错，也不保证每轮睡眠都切入 idle；不能把 litmus 通过当成形式化证明。现有四类内核 MM 切换和 CPU lease 回收测试继续独立运行。

### 5.10 缺口补充验证

本轮在 `d467030dcb` 基础上补充验证，没有恢复性能实验或本地完整 clippy。取消节点、epoch grace、唯一创建令牌和 RAII 资源事务的独立只读核验未发现已证实的 UAF、重复释放或丢失取消；这属于静态核验记录，不代替合入前维护者的调度及 unsafe 审查。

| 检查 | 结果与日志 |
| --- | --- |
| 格式与差异 | `cargo fmt -- --check`、Starry include 子模块定向 rustfmt、`git diff --check` 通过 |
| 增量 std | `cargo xtask test --since d467030dcb` 的 13 包全部通过；`/tmp/pr2357-gap-std.log` |
| 定向 clippy | `ax-task`、`ax-runtime`、`arceos-test-suit` 共 72 组合通过；`/tmp/pr2357-gap-clippy.log` |
| ArceOS `task-wait-queue` | 四架构通过，包含实际 CPU 周期、预留释放及持 MM 锁回归；`/tmp/pr2357-gap-final-idle-<arch>.log` |
| Starry kernel axtest | 四架构通过，包含拒绝下线后的 active-MM 寿命；`/tmp/pr2357-gap-final-kernel-<arch>.log` |
| Starry `qemu/system/syscall-test-mm-lifecycle` | 四架构通过；`/tmp/pr2357-membarrier-user-{x86_64,riscv64,loongarch64}.log`、`/tmp/pr2357-membarrier-isolated-aarch64.log` |

AArch64 首次构建被共享 target 中的 `bitmaps@3.2.1` future-incompat 报告门禁拒绝；保留门禁，以独立 `CARGO_TARGET_DIR=/tmp/pr2357-gap-aarch64-target` 重跑后通过。没有删除共享报告或扩展允许列表。新增探针只恢复既有 hook 契约，未改变体系结构切换或平台启动协议。


### 5.11 完整套件的 CPU 下线前提

提交 `d06e05dbf1` 的 CI 在 ArceOS x86_64 `all` 中失败于 `released reservation must permit idle CPU cycle`，本地同一入口也复现。此前单独 `task-wait-queue` 通过不能证明完整套件的 CPU 已排空：`all` 启用 fs/net，`BlockThreadOps::spawn_pinned` 和网络队列 executor 仍保有固定 CPU 服务线程。释放测试线程的 activation 预留并不撤销这些服务的 CPU 所有权。

Linux v7.1 RT 的依据是 `kernel/sched/core.c::sched_cpu_deactivate` 先清除 active mask，启动 `balance_push` 并执行 `synchronize_rcu`；`kernel/cpu.c` 的反向 hotplug 状态遍历在 `sched_cpu_wait_empty` 之前调用 `smpboot_park_threads`。`block/blk-mq.c::blk_mq_hctx_notify_offline` 先关闭对应 hctx 准入，再等待在途请求。`balance_hotplug_wait` 还检查 RT 的 `rq_has_pinned_tasks`；`sched_cpu_dying` 不允许未排空的 CPU 直接退出。当前 `take_cpu_offline` 是要求调用方先排空的最终事务，不包含 Linux 上述设备与线程的完整停放编排，不能把 blocked worker 当作已经 parked。

CPU 周期和全局 MM 锁回归迁到 `task/cpu_lifecycle.rs` 的独立 `task-cpu-lifecycle` feature。`ARCEOS_RUST_STANDALONE_FEATURES` 将它与 `task-irq` 一起加入默认运行，四架构 CI 的既有 `cargo xtask arceos test qemu --arch <arch>` 因而仍必跑这项回归。`all` 继续覆盖其他生命周期与设备测试；成功下线、取消不执行、预留阻止下线和 MM 锁断言均保留。独立用例明确要求至少三个 CPU，不通过 UP 跳过产生成功结果。

该修复只更正测试执行前提，不新增或放宽生产热插拔规则。Linux 完整设备停放、迁移和物理 CPU 热插拔仍是明确未实现的能力，不以此次 CI 修复冒充完成。
