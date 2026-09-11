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
| vfork / X58；G 使用 clone | [v7.1 固定提交 fork.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/fork.c) | 父线程等待子进程 exec 或退出 | `sys_vfork → sys_clone(VFORK|VM) → Thread::notify_vfork_done` | 部分正确 | 原普通路径四架构通过；新发现的 MM 通知时机和共享 SHM 缺口见第 5.29 节 |

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

`request_idle_cpu_round_trip` 先发布不可消费的 `ARMING`，投递真实 scheduler work；生产者租约退出后发布可消费的目标 CPU。后续第 5.12 节修复最终入睡检查，让 ARMING/ready 请求持续阻止 idle 入睡，不依赖第二次物理 IPI。完成结果与消费状态放在同一个原子字中，避免旧消费者清除下一次请求。`idle_cpu_reservation_round_trip` 验证 staged 预留拒绝下线，取消或激活并退出后允许下线/上线，并用目标 CPU 上的实际睡眠验证恢复后的调度和 clockevent。

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


上线后验证 clockevent 的线程也必须先结束物理执行：`ThreadHandle::join` 只等待逻辑完成，回收忙时允许转交 reaper。CPU 周期测试现在等待该线程的 `execution_reclaimed`，再发下一次下线请求，避免把逻辑退出当成 Linux `sched_cpu_wait_empty` 的排空条件。首次 AArch64 默认入口在 MM 锁回归返回 `Some(false)`，补齐这一等待后同一用例通过；没有添加超时重试。


最终四架构默认 Rust 入口各通过 `all`、`task-irq`、`task-cpu-lifecycle`，共 12 个运行，日志 `/tmp/pr2357-ci-final-<arch>.log`。定向 clippy 的 axbuild 与 ArceOS 测试包 42 个组合通过；增加物理回收等待后，测试包 41 个组合再次通过。`cargo xtask test --since d06e05dbf1` 运行 axbuild，783 项通过，包含默认入口选择回归；fmt 与差异检查通过。原始 CI/本地失败证据分别在 `/tmp/pr2357-ci-x86-failure.log`、`/tmp/pr2357-ci-all-red.log`，没有用重跑旧实现代替修复。


### 5.12 idle 请求与最后入睡检查

`1545558b68` 的远端 AArch64 CI 在 `task-cpu-lifecycle` 超时。测试邮箱的请求不是普通 runnable task；仅发送物理 IPI 不能使其成为 scheduler 的持久 need-resched 条件。IPI 若在 service 点之后、最终 WFI 检查之前被处理，原来的调度状态检查可能看不到这项测试工作。Linux `kernel/sched/idle.c::do_idle` 和 `default_idle_call` 明确要求在 IRQ-off 入睡边界重检工作，不能靠已经消耗的中断边沿证明可睡眠。

仅在 `fault-injection` 下，最终 idle wait 事务现在同时观察测试邮箱。ARMING、延迟到 wait 边界发布和 ready 均阻止 WFI；DONE 不阻止。生产者仍先投递普通 owner work，退出 publication 租约后才发布可消费状态，取消多余的直接物理 IPI。正常生产构建没有测试邮箱或这条分支。

`request_idle_cpu_round_trip_at_wait` 在真实 idle service 点之后、IRQ-off wait 边界把同一邮箱请求变为可消费。测试断言这种确定性交错不能进入 WFI：旧实现失败于 `idle must recheck pending owner probe before WFI`，新实现返回 idle 主循环并执行同一下线事务。红绿日志为 `/tmp/pr2357-idle-pending-{red,green}.log`；断言只约束本次边界注入，后续并发请求仍由正常 IRQ-off 通知协议唤醒。

### 5.13 创建分配与失败回滚

固定 Linux 基准的 `copy_process` 在 `dup_task_struct` 失败时返回 `-ENOMEM`，调度初始化完成后才允许发布与首次唤醒。创建路径不能因为“节点在首次入队之前准备”就在持有 raw rq 锁时分配。此前 `prepare_thread_slot` 会在 IRQ-save rq 锁内扩容各调度类的索引，本轮删除该入口及转发，把这些存储移到 `TaskSystem` 初始化。

`RunQueue::configured` 按 `TaskSystemConfig::thread_capacity` 预建 membership、Fair、Deadline 和 pushable 索引；登记槽、空闲槽和退出候选也在锁对象投入使用前预留容量。创建、唤醒及迁移不再扩容这些索引。代价是每 CPU 提前承担配置上限对应的索引内存，而不是随活跃线程数增长；配置上限仍限制可用线程槽，槽耗尽与堆分配失败保持不同错误。

`thread::allocation` 使用标准 `Box::try_new`、`Arc::try_new` 和 `Vec::try_reserve_exact`。入口闭包、公共执行对象、调度状态、PI/运行队列节点及任务核心均沿 `Result` 返回 `RuntimeFailure(NoMemory)`。显式 affinity 移交所有权后由一个 `Arc<CpuSet>` 共享，不再在创建途中隐式克隆底层向量。没有引入自定义引用计数或可变尾部布局。

`ThreadSlotReservation` 统一拥有未发布槽的 generation 和 Deadline 准入额度。私有对象分配失败时，先撤销槽与额度，再由 `UnpublishedThreadGuard` 释放资源及直接 OS extension；bind 或提交重验证失败也先释放预留锁，再执行资源析构。只有登记表提交成功才解除回滚责任，失败路径不运行新线程入口。

运行时为栈描述符先分配 `Box<MaybeUninit<RuntimeStack>>`，随后取得栈 backing，成功后用 `Box::write` 完成对象；这样描述符分配失败不会遗失 backing。`TlsArea::try_alloc`、架构上下文及 MM owner/token 的包装分配同样可失败。已取得的 TLS、栈、context 和 MM 由既有所有权事务回收。一次性 bootstrap 的致命失败策略仍明确保留，不能据此宣称所有系统初始化分配都已可恢复。

`ThreadAllocationProbe` 只在 `fault-injection` 构建中为当前创建者注入分配边界失败。ArceOS 在 Fair 和高利用率 Deadline 两种策略下逐个失败，检查 ENOMEM、闭包与扩展各析构一次、入口不执行，并立即成功创建下一线程以检测准入泄漏；当前带 TLS 的 x86_64 路径各覆盖 24 个边界。Starry 从 MM owner 装配到用户上下文准备逐个注入，验证 MM owner 恰好释放一次。这是实际生产路径的分配边界注入，不是耗尽整机堆的实验。

旧 rq 路径由同一上下文断言确定性报出 `thread allocation must not hold a scheduler or IRQ lock`，日志 `/tmp/pr2357-allocation-rq-red.log`；预分配后四架构默认 ArceOS 套件通过，日志 `/tmp/pr2357-alloc-default-<arch>.log`。本轮资源实现的定向 clippy 覆盖 ax-task、ax-runtime、ax-hal、ArceOS 测试共 85 个组合；Starry 四架构 kernel axtest 均通过。槽回滚与运行时 OOM 所有权的两项独立静态核验未发现已证实的泄漏或重复释放；这不替代合入前维护者的调度及 unsafe 审查。

错误传播回归在实际准备失败后同时通过 ArceOS `ApiError → IoError` 和 Starry `StarryError::linux_errno`，旧映射确定性得到 `(BadState, EFAULT)`，修复后同一断言得到 `(NoMemory, ENOMEM)`。两个边界只为 `RuntimeStatus::NoMemory` 增加精确分支，其他运行时故障保留原分类。x86_64 红绿证据为 `/tmp/pr2357-oom-errno-{red,green}.log`；clone 既有专用转换原本已保留分配失败的 ENOMEM。

### 5.14 idle 边界测试的下线条件

`6ebdd372eb` 的 CI 在 RISC-V `task-cpu-lifecycle` 报出 `idle boundary publication must be serviced`。该断言收到的是已完成的 `Some(false)`，并非请求没有被消费；它把处理测试邮箱与 CPU 此刻能够成功下线合成了一个条件。`take_cpu_offline` 还要求 publisher、owner work、idle-pull、定时器和任务目标排空，`CpuLocal/CpuRemote::is_quiescent_for_offline` 并不由一次 idle 边界检查保证。Linux `do_idle` 的工作重检和 `sched_cpu_wait_empty` 的下线排空也是不同协议，不能为测试放宽后者。

WFI 边界用例现在持有真实 staged 预留，使结果有明确前提：边界请求必须被消费，但下线必须拒绝，且入口不能执行。测试随后取消预留并等待执行资源回收。前面两组预留释放后的成功 offline/online、CPU 归属与 clockevent 检查保持不变。用同一 staged 输入运行原来的成功断言，确定性得到失败；改为检查预留不能被绕过后通过。日志为 `/tmp/pr2357-idle-boundary-contract-{red,green}.log`。没有改变生产调度器下线条件，也没有增加重试或超时。

补充反向验证临时移除 runtime 的最后 pending 检查，同一个新测试仍确定性触发 `idle must recheck pending owner probe before WFI`，日志 `/tmp/pr2357-idle-boundary-wfi-red.log`；随后恢复生产实现。这证明预留输入没有绕过原先受保护的 WFI 边界。

分配提交 `6ebdd372eb` 的最终错误映射复测四架构通过：AArch64 179 项，其余各 178 项，日志 `/tmp/pr2357-oom-errno-green.log`、`/tmp/pr2357-oom-errno-riscv64.log`、`/tmp/pr2357-oom-errno-final-{aarch64,loongarch64}.log`。`cargo xtask test --since 3c7f1c69d4` 的 13 包全部通过。补充 ax-api/Starry 定向 clippy 在第 85/101 项因构建输出目录缺失中断，不能记为全通过；此前第 5.13 节的 85 个资源实现组合与本次补充检查是不同记录。完整静态门禁继续交给 CI，未扩大为本地全工作区 clippy。

最终四架构 `task-cpu-lifecycle` 全部通过，日志 `/tmp/pr2357-idle-boundary-final-<arch>.log`。同一 RISC-V 默认入口复测 `all/task-irq/task-cpu-lifecycle` 3 项通过，日志 `/tmp/pr2357-idle-boundary-default-riscv64.log`。原始远端失败日志为 `/tmp/pr2357-6eb-riscv-failure.log`；这些证据区分请求已处理、预留拒绝和真正排空后的成功下线。

### 5.15 Starry 扩展的前置分配

`prepare_user_thread_inner` 在调用公共 builder 前仍需创建 `StarryUserTaskExtension`，因此核心创建对象的可失败分配并不覆盖这层 OS 状态。该入口改用 `Box::try_new`；共享线程名采用 `Arc<String>`，先通过 `String::try_reserve_exact` 复制内容，再使用 `Arc::try_new` 建立共享快照。失败沿既有 `TaskError::RuntimeFailure(NoMemory)` 返回，尚未移交的 `Thread`、MM 配置与闭包按 Rust 所有权释放。没有引入可变尾部对象或自定义引用计数。

`UserTaskRef::name` 和 extension 字段同时迁移到新的快照类型，原有名称读取调用方由内核构建检查覆盖；名称内容及锁外构造、锁内替换、锁外释放旧快照的顺序保持不变。`StarryContextState` 的 MM 由所有构造路径必填，已删除无调用方的 `None` 分支；默认亲和性不再提前构造全 CPU 掩码，交给公共 builder 的可失败默认分配，继承亲和性继续作为显式覆盖。此处清理没有增加内核上下文分支。这一改动只补齐用户线程装配的前置分配，不宣称 `Thread::new`、进程资源复制及名称更新等其他入口的全部分配已具备 OOM 回滚。

`task_name_allocation_failure_preserves_snapshot` 在真实 kernel axtest 中逐一注入字符串存储及共享快照分配失败。旧实现确定性触发 `name allocation failure must return ENOMEM`，新实现返回 ENOMEM，旧快照保持有效，随后正常构造成功。`unpublished_extension_allocation_releases_process` 使用真实 PID 预留、Thread 和 ProcessData，在装配的三个分配边界失败，检查入口不能执行、错误与尝试次数正确、进程没有被残留 extension 引用保留。它不替换调度运行时，也不模拟整机堆耗尽。

x86_64 的同一名称回归红绿日志为 `/tmp/pr2357-starry-name-{red,green}.log`；加入 extension 回滚后 x86_64 kernel axtest 180 项通过，日志 `/tmp/pr2357-starry-extension-green.log`。RISC-V 同一内核套件通过，日志 `/tmp/pr2357-starry-extension-riscv64.log`。随后当前源码的四架构最终 kernel axtest 全部通过：AArch64 181 项，其余各 180 项，日志 `/tmp/pr2357-extension-final-<arch>.log`。Starry 定向 clippy 92/92 通过，日志 `/tmp/pr2357-starry-extension-clippy.log`；未运行本地完整工作区 clippy。LoongArch 下线回归的 `Some(false)` 仍未定位具体拒绝条件，本节分配修复不作为该 CI 故障的修复证据。

### 5.16 下线拒绝的诊断边界

`de61c97784` 的 push CI 在 LoongArch 持全局 MM 锁用例中返回 `Some(false)`，并非等待超时。原日志无法区分 placement publisher、线程目标、owner publisher、本地 rq/timer/handoff 状态与线程迁移所有权，因此不能由这一失败推导出全局 MM 锁反转。相同源码的补充 CI 四架构 ArceOS 套件随后通过，也不能替代根因分析。

仅在 `fault-injection` 构建中，`IdleOfflineRejection` 记录最后一次串行 idle 探针失败的判定阶段。记录在原判断处完成，日志在 `probe_idle_cpu_round_trip` 返回、其 IRQ/owner 与登记表锁全部释放后输出。非测试构建没有诊断原子变量；原短路判断顺序和取消 deactivation/draining 的顺序保持不变，不重试、不放宽排空条件，也不将拒绝记作成功。

本地 LoongArch `task-cpu-lifecycle` 通过，三次 staged 预留均记录为 `PlacementPublication`，日志 `/tmp/pr2357-offline-rejection-probe.log`。这只验证诊断路径与原断言同时生效，没有复现 CI 的非预期拒绝；该根因仍未关闭。

### 5.17 Thread 私有状态的可失败构造

`Thread::new` 原先在调度器装配之前创建 CPU 时间、退出状态、seccomp 和信号对象，仍可能因不可失败堆分配而终止内核。它现在返回 `StarryResult<Thread>`；clone 使用既有错误链传播失败，init 则在一次性启动边界明确处理致命失败。公共执行生命周期仍由 ax-task 持有，Starry 的退出、信号和凭据字段保留其 Linux ABI 责任，没有合并两者状态。

`task::allocation` 集中私有线程对象的标准 `Arc::try_new` 和向量预留，供 CPU 时间、退出状态、seccomp 与扩展装配使用。移除 `CpuTimeAccounting` 的不可失败 `Default`，调用方直接消费可失败构造。seccomp 的首个快照和保留向量都准备成功后才安装裸指针；快照读者的既有生命周期协议保持不变。

`ThreadSignalManager::new/new_with_blocked` 同样返回 `SignalResult`。进程的 `register_child` 保留锁外预留、锁内重新验证和发布的顺序，预留失败返回 `SignalError::NoMemory`，在 Starry 错误边界精确映射到 ENOMEM。Thread 的其他私有分配先完成，信号登记最后执行；这样在登记之前失败不产生提前可见的弱引用。没有修改普通信号投递、等待或 RT 锁的语义。

固定 Linux v7.1 的 `copy_process` 对各项复制失败逐层回滚，`copy_signal` 分配失败返回 `-ENOMEM`。下面只评估本次新增分配错误链；正常 clone 的既有系统测试不能证明用户态 OOM 注入行为。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| clone / x86_64 56；asm-generic 220 | [v7.1 固定提交 fork.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/fork.c) | 未发布线程的私有状态分配失败返回 ENOMEM并撤销资源 | `do_clone_in_cgroup → Thread::new → ThreadSignals/ThreadLifecycle/ThreadAccounting/ThreadSecurity`；信号错误集中经 `StarryError::linux_errno` 转换 | 部分正确 | 真实 kernel axtest 对构造路径逐分配边界注入，原实现确定性未返回 ENOMEM；尚无直接用户 syscall 级 OOM 注入证明，也不据此声称整个进程复制链已覆盖 |

`thread_state_creation_returns_allocation_failure` 先观察真实构造经过的分配边界，再逐个注入失败，检查 ENOMEM、尝试次数以及进程强所有权释放。测试使用真实 PID 预留与 ProcessData，不替换 TaskRuntime。原实现的确定性失败日志为 `/tmp/pr2357-thread-state-red.log`；首次修复后的 x86_64 kernel axtest 181 项通过，日志 `/tmp/pr2357-thread-state-green.log`。逐边界最终版本的多架构验证另行记录，不能以第一次单边界通过替代。

最终逐边界版本的四架构 kernel axtest 全部通过：AArch64 182 项，其余各 181 项，日志 `/tmp/pr2357-thread-state-final-<arch>.log`。信号组件定向 clippy 通过，日志 `/tmp/pr2357-thread-signal-clippy.log`。组件的 `axtest` 条件显式加入 check-cfg，未降低告警等级；内核其余功能组合继续由 PR CI 检查。本节不将先前未覆盖的整个进程地址空间、文件表或命名空间复制分配算作已完成。

### 5.18 创建错误的分类

`map_task_creation_error` 原先把线程槽耗尽映射为 EFAULT，并把所有 RuntimeFailure 统一映射为 ENOMEM。现在 `ThreadCapacity` 返回 EAGAIN，只有 `RuntimeStatus::NoMemory` 返回 ENOMEM，其他运行时故障保留 BadState/EFAULT 分类。固定 Linux `copy_process` 在线程数量限制检查前设置 `retval = -EAGAIN`，与分配失败的 ENOMEM 明确分开。

`creation_errors_preserve_resource_domain` 调用实际 clone 错误转换及最终 errno 边界：旧实现确定性返回 `[EFAULT, ENOMEM, ENOMEM]`，修复后同一输入得到 `[EAGAIN, ENOMEM, EFAULT]`。x86_64 kernel axtest 红绿日志为 `/tmp/pr2357-clone-errors-{red,green}.log`。这证明转换边界，未用其代替直接用户态线程上限或 OOM 故障注入的系统证明。

### 5.19 补充系统调用证据

在 `b31727b024` 上执行现有 x86_64 定向系统测试，没有新增重复用例。`bugfix-clone3-badsize` 直接调用 syscall 435，验证零填充的未知结构扩展能够创建并等待子进程；`zombie-bugfix-bug-waitid-basic` 直接调用 waitid，覆盖 P_PID/P_ALL/P_PGID、当前进程组、WNOHANG/WNOWAIT、回收与错误输入；`test-queued-affinity` 通过实际 syscall 验证排队任务的亲和性迁移及恢复。三项均出现各自 `STARRY_SYSTEM_TEST_PASSED` 标记，日志 `/tmp/pr2357-plan-audit-<case>.log`。

这些证据更新第 4 节早期“本次未执行专项”的时间范围，但不将单项场景推广为每个 flag、每种 OOM 或所有体系结构的完整证明。直接 fork、其余策略设置场景以及 syscall 级 OOM 注入仍按各自证据独立评估。

### 5.20 信号登记的最终释放

`ProcessSignalManager::children_snapshot` 原先仅在后续信号扫描时清理失效弱引用。线程退出或创建回滚后，长期不接收信号的进程仍会保留这些登记与 Arc 分配存储。现在 `ThreadSignalManager::drop` 在最后强引用退出时主动注销，不依赖一次无关的未来信号。

登记匹配使用被引用保护的对象地址，防止旧清理路径删除复用同一 TID 的新对象；TID 可能因 exec 改变，不作为析构时的匹配条件。地址只比较、不解引用。进程由正在析构对象的 Arc 字段保持有效，Arc 的隐含弱引用持续到析构完成，因此取走登记不会提前释放正在析构的对象。`unregister_child` 在原登记锁内移出弱引用，在锁外丢弃；后续扫描对同一个登记再次清理也是无副作用的。

Linux 固定基准的 `__exit_signal → __unhash_process` 主动删除线程组登记，再按独立读者/对象回收协议释放资源。这里不复制 Linux 的 RCU 对象布局，也不以 Arc 计数替代宽限期；信号快照已有强引用保持当前读者对象有效，移除弱登记只停止后续快照发现它。

`last_thread_owner_unregisters_without_an_unrelated_signal` 在旧实现确定性失败于登记仍存在；修复后同一用例验证立即注销以及旧对象清理不影响复用 TID 的新对象。`cargo xtask test --since d563c3477c` 红绿日志为 `/tmp/pr2357-signal-retire-{red,green}.log`，修复后信号与内核两包 std 检查通过；信号组件定向 clippy 通过，日志 `/tmp/pr2357-signal-retire-clippy.log`。真实内核回归结果单独记录，不以组件测试冒充 IRQ/切换行为证明。

新增注销路径后的 x86_64 kernel axtest 全部通过，日志 `/tmp/pr2357-signal-retire-kernel.log`，继续覆盖 Thread 私有分配回滚、扩展回滚和实际切换/回收。该结果不表示此前 SG2002 停滞或 LoongArch 非预期下线拒绝已定位根因。

### 5.21 exec 更名后的登记回收

独立核验发现 `536505f3ba` 把创建时 TID 同时保存在 `ThreadSignalManager`，而非主线程 exec 的 `rename_child` 只更新进程登记表。最后强引用析构时，旧 TID 无法匹配已更名的表项，重新出现等待无关信号才释放弱登记的问题。固定 Linux 基准的 `fs/exec.c::de_thread` 用 `exchange_tids/transfer_pid` 转交身份，`kernel/exit.c::__unhash_process` 按任务对象解除登记；对象生命周期不以最初的数字 TID 为准。

现在删除 manager 内重复的 TID，`unregister_child` 按对象地址匹配。析构期间 Arc 隐含弱引用、扫描期间显式 Weak 各自阻止该分配地址复用；不增加原子 TID、额外锁或新引用计数。取走登记仍在原锁内，最终 Weak 释放仍在锁外。

`renamed_thread_owner_unregisters_after_exec` 创建线程、执行真实 `rename_child`，再创建复用旧 TID 的对象；释放更名线程必须只留下新对象，最后释放新对象后登记表必须为空。同一测试在旧实现确定性得到两个残留登记而非一个，日志 `/tmp/pr2357-signal-rename-red.log`。该组件回归直接观察资源登记责任；用户态 exec 的成功只能补充身份转交行为，不能单独证明内核弱引用已释放。

远端 `b31727b024` 的 [CI 34470614083](https://github.com/rcore-os/tgoskits/actions/runs/34470614083) 已结束：增量 clippy、std、ArceOS/Starry 四架构和 Starry 板卡通过；ASUS NUC15CRH 失败且部分 Axvisor 作业取消。按用户要求排除 NUC，取消项仍缺验证。SG2002 此轮通过不等于先前停滞已修复，LoongArch 同理。

修复后同一组件回归通过，两包增量 std 与信号组件定向 clippy 通过，日志 `/tmp/pr2357-signal-rename-{green,clippy}.log`。x86_64 四核真实 `qemu/system/test-thread-lifecycle-exec` 通过，日志 `/tmp/pr2357-signal-rename-exec.log`，直接 SYS_execve 验证非 leader 更名、原 PID 保持及父进程等待。execveat 共用 `do_execve` 的注销路径，但本轮未单独运行 execveat 更名场景。

### 5.22 内核服务创建入口

Starry 原先仍通过 `spawn_kernel_thread`、`spawn_kernel_thread_with_stack/affinity/policy_and_affinity`、两种 `try_spawn_kernel_thread`、`join_kernel_thread` 和重复默认栈 getter 装配内核服务。这些入口已经转发给公共生命周期，但继续以组合函数重复表达 builder 参数。本轮迁移所有调用方，仅保留 `kernel_thread_builder` 设置 Starry 默认栈大小，返回原来的 ax-task `ThreadBuilder`；没有新增另一种 builder 或管理句柄。

stopper/perf/cpufreq 服务在 `spawn` 前使用 `policy/affinity`，普通服务使用默认配置；evdev、KPU、AXIVC 与 timer worker 的创建错误继续由原调用方撤销启动状态。等待直接消费 `ThreadHandle::join`，后台服务仍由调度器拥有执行责任。闭包内容、唤醒协议、IRQ 状态和发布顺序保持原样，本节是独立接口迁移，不作为历史板卡故障或 OOM 缺口的修复证据。

这项迁移复用既有真实 kernel axtest 与 CI 功能矩阵，不增加只检查转发关系的测试。全仓 Rust 检索确认 Starry 的上述旧入口已无定义或调用；ArceOS runtime 自身的默认栈配置仍由其平台装配层维护。

迁移后的 x86_64 kernel axtest 共 182 项通过，无失败或跳过，日志 `/tmp/pr2357-kernel-builder-ktest.log`。本次未新增内核行为，因此没有为单纯转发接口构造人工红绿；调度、退出与资源回收继续由已有真实运行时回归约束。两包增量 std 全部通过，日志 `/tmp/pr2357-kernel-builder-std.log`；Starry 定向 clippy 四架构 92/92 组合全部通过，日志 `/tmp/pr2357-kernel-builder-clippy.log`；未执行本地全工作区 clippy。

### 5.23 固定 dev 提交的 CI 复核

复核沿用首次调查保存的 15 个 dev 提交，以 `81d50f67ac` 为清单顶端，不把调查期间新合入的提交替换进来。GitHub 当前查询得到 7 轮失败、6 轮成功、1 轮取消、1 个提交无 CI 记录。运行状态只证明对应提交的 CI 结果，不能单独确定错误归属。原始清单、运行及作业元数据保存在 `/tmp/ax-task-dev15/{commits,runs,jobs}.json`。

| dev 提交 | CI 运行 | 当前终态 |
| --- | --- | --- |
| `81d50f67ac` | [34430391363](https://github.com/rcore-os/tgoskits/actions/runs/34430391363) | failure |
| `cee67bd09a` | [34428429562](https://github.com/rcore-os/tgoskits/actions/runs/34428429562) | failure |
| `b2a3682a7b` | [34424085211](https://github.com/rcore-os/tgoskits/actions/runs/34424085211) | success |
| `3e751a68b8` | [34418485986](https://github.com/rcore-os/tgoskits/actions/runs/34418485986) | cancelled |
| `ac8eaf7bb7` | [34418199371](https://github.com/rcore-os/tgoskits/actions/runs/34418199371) | failure |
| `cc3db295c8` | [34368351913](https://github.com/rcore-os/tgoskits/actions/runs/34368351913) | success |
| `1027176f76` | [34346970094](https://github.com/rcore-os/tgoskits/actions/runs/34346970094) | success |
| `bbf4772685` | [34340537243](https://github.com/rcore-os/tgoskits/actions/runs/34340537243) | success |
| `63365ffb7c` | [34339553573](https://github.com/rcore-os/tgoskits/actions/runs/34339553573) | failure |
| `ad49842255` | [34332660124](https://github.com/rcore-os/tgoskits/actions/runs/34332660124) | failure |
| `504808b73b` | [34329952009](https://github.com/rcore-os/tgoskits/actions/runs/34329952009) | failure |
| `581c44ad98` | [34327850888](https://github.com/rcore-os/tgoskits/actions/runs/34327850888) | success |
| `cc8faa9222` | 无记录 | 无法确认 |
| `cdfe00c678` | [34325466260](https://github.com/rcore-os/tgoskits/actions/runs/34325466260) | success |
| `d64451917e` | [34321235630](https://github.com/rcore-os/tgoskits/actions/runs/34321235630) | failure |

失败日志中，`102720076216` 的 AArch64 `block_runtime_async_double_read` 触发旧 CID 不等断言，已由第 5 节的独立 DMA 所有权回归修正；不能因 CID 相同认定调度错误。`102689248865` 的 x86_64 `test-uid-gid-re-setters` 在 NPTL 并发 setreuid 阶段超时，前两项已通过，尚无阻塞任务或唤醒链证据，继续保留为待定位项。

`102449958281`、`102449957971` 在 rootfs 下载时 HTTP 500，`102462774825` 在获取镜像 registry 时连接超时，均未进入对应 guest 测试。robot 的 `102728943741`、`102439317535` 直接失败于 28 FPS 门槛，`102689249124` 第二次运行无帧后超时；伴随 xHCI/USB 错误但不足以归因调度，性能处理仍按用户要求暂停。NUC 作业 `102401877640` 按要求排除。未改变阈值、重试次数或测试断言。

日志源码版本以 checkout 后的 `git log -1 --format=%H` 为准；日志中的 `a69a63265...` 是 Rust 工具链版本，不是 TGOSKits 提交。全部八份非 NUC 失败日志已下载为 `/tmp/ax-task-dev15/job-<id>.log`；下载连接失败后补取的是同一作业日志，不是重新运行测试。

在本地 `1837ad4cc8` 执行原 `qemu/system/syscall-test-uid-gid-re-setters`，x86_64 四核通过，三项 NPTL 同步均通过，日志 `/tmp/pr2357-dev15-nptl-current.log`。这次没有修改测试或凭据/信号实现，因而只证明当前单次运行通过，不作为原超时已经定位或修复的证据。

### 5.24 用户上下文的具名装配

`prepare_user_thread` 现在统一接收入口、`Thread` 和 `UserThreadOptions`，移除两个带 FP/scheduler-state 名称的组合入口及内部 `StarryContextState`。配置持有线程名称、初始调度状态及架构 FP 初始化选择，MM 仍由统一入口从 `Thread::proc_data` 取得，调用方不能通过配置传入另一进程的 MM。所有现有 Starry 用户线程均使用相同的 `KERNEL_STACK_SIZE`，因此删除无独立调用需求的栈大小位置参数，沿公共 builder 装配。

init 使用默认调度与初始 FP 状态；clone 保留策略、亲和性及 reset-on-fork 的既有计算。RISC-V 仍在原位置保存 FP 寄存器并设置 FS，之后传入 `with_fp_state`；其他架构只选择 `inherit_current_fp`，实际保存继续由 ax-runtime 在原准备阶段执行。固定 Linux 的 `arch_dup_task_struct` 也分别维护 arm64 FPSIMD 和 LoongArch FP 所有者，不能将配置收敛解释为取消架构差异。

本轮没有修改 `stage → PID/拓扑发布 → activate`、错误转换或 FP 寄存器保存代码。既有 extension 分配回滚测试使用同一配置路径，真实四架构 `test-clone-fp-state` 验证子线程首次执行时的寄存器状态。保留的 runtime 同名 `prepare_user_thread(builder, entry, UserContextOptions)` 是架构资源装配接口，与 Starry OS 配置承担不同责任。

统一入口后 x86_64 kernel axtest 182 项通过，四架构原 `test-clone-fp-state` 全部通过，均有实际子线程寄存器断言和 `STARRY_SYSTEM_TEST_PASSED` 标记。日志为 `/tmp/pr2357-user-options-kernel-x86_64.log`、`/tmp/pr2357-user-options-fp-<arch>.log`。fmt 和差异检查通过，该用户入口版本的增量 std 和 Starry 定向 clippy 92/92 均通过，日志 `/tmp/pr2357-user-options-{std,clippy}.log`；与前一内核服务迁移的验证分开记录。

### 5.25 独立目标的失败收集

`dd8f1c9347` 的 CI 第一轮已结束：ArceOS/Starry 四架构、Starry 板卡、std 与增量 clippy 通过；NUC 失败令 Axvisor 同组 10 项取消。用户明确排除 NUC 的排查，但 fail-fast 仍会终止其他目标，使每次新推送都缺少独立验证结果。

Axvisor 调用既有 reusable matrix 时改为 `fail_fast: false`，保留每个作业及整轮工作流原来的失败状态，不修改测试、超时、退出码或成功匹配。其他 CI 分组仍保持原 fail-fast 策略。`check_ci_routing.py` 同步明确要求这一分组策略，未删除路由校验；实际矩阵执行器仍直接使用 `inputs.fail_fast`。

旧路由规则在新的工作流配置上明确拒绝 Axvisor 分组；更新策略后，`python3 scripts/test/check_ci_routing.py` 通过，`python3 -m unittest scripts.test.test_ci_routing` 的 26 项通过，日志 `/tmp/pr2357-ci-failure-collection-tests.log`。这只证明配置和路由，独立目标能否完成仍需新提交 CI 的真实结果。

### 5.26 TSYNC 的限制标志传播

固定 Linux `kernel/seccomp.c::seccomp_sync_threads` 同步过滤器时也传播调用线程的 `no_new_privs`，防止线程组通过安装过滤器的线程退出而绕过该限制。Starry 的 `sync_seccomp_to_thread_group` 原先只调用 `set_seccomp_state`。现在保存调用者的 NNP 状态，在开启对端 seccomp syscall-work 之前设置其单向限制标志。

现有 `syscall-test-seccomp` 的 TSYNC 用例增加对端 NNP 的 0→1 断言，用 C11 acquire/release 握手替代原 `volatile` 通知，保证对端先确认初态，源线程再设置 NNP 并执行 TSYNC。原实现确定性通过过滤器断言却失败于 NNP 传播，修复后同一 x86_64 四核用例通过，日志 `/tmp/pr2357-seccomp-nnp-{red,green}.log`。宿主测试进程已经继承 NNP=1，无法构造初态 0，因此 `/tmp/pr2357-seccomp-nnp-linux.log` 不计为有效的 0→1 对照；依据仍为固定 Linux 源码和真实 Starry 红绿。

下表只评估本次标志传播，不将其扩大为 seccomp 全部 TSYNC、clone 或 exec 语义正确。X 表示 x86_64，G 表示 AArch64、RISC-V64、LoongArch64。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| seccomp(TSYNC NNP) / X317 | [固定 seccomp.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/seccomp.c) | 同步过滤器与源线程 NNP | `sys_seccomp → sync_seccomp_to_thread_group → Thread` | 正确 | 同一原始 syscall 用例 x86_64 红绿 |
| seccomp(TSYNC NNP) / G277 | [固定 seccomp.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/seccomp.c) | 同上 | 共用上述实现 | 正确 | RISC-V、AArch64、LoongArch 同一系统用例通过 |
| prctl(GET_NO_NEW_PRIVS) / X157 | [固定 sys.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/sys.c) | 对端查询同步后的单向标志 | `sys_prctl → Thread::no_new_privs` | 正确 | 原始 prctl 查询对端 0→1 |
| prctl(GET_NO_NEW_PRIVS) / G167 | [固定 sys.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/sys.c) | 同上 | 共用上述实现 | 正确 | RISC-V、AArch64、LoongArch 同一系统用例通过 |

本轮两包增量 std 与 Starry 定向 clippy 四架构 92/92 已通过，日志 `/tmp/pr2357-seccomp-nnp-{std,clippy}.log`。

此阶段的独立源码核验还确认一个发布竞争（后续修复见第 5.27 节）：clone 在 `Thread::new` 后复制父线程安全状态，随后才执行 PID 发布与 `Process::add_thread`；TSYNC 的线程集合快照与这些操作没有共同锁。Linux `copy_process` 在共同 sighand 锁下执行最后一次 `copy_seccomp` 并加入线程组。NNP 传播修复不关闭这个旧过滤器逃逸窗口，后续必须统一“最后继承→发布”和 TSYNC 的串行边界；不能以对端标志回归通过替代它。

TSYNC 新断言最终四架构均通过，日志 `/tmp/pr2357-seccomp-nnp-{green,riscv64,aarch64,loongarch64}.log`。

`32820bc112` 的 OrangePi 作业 `102879230767` 再次在 native-hardware-smoke 超时。新日志明确记录 `rknn_yolov8_demo`、`yolov8.sh` 和输出文件的 `cat` 均以 0 退出，随后启动另一个命令但未完成整个 shell 检查。不能将这一失败定位为 MemDestroy 或 NPU 未完成；根因仍需后续命令与终端/等待状态证据，日志 `/tmp/pr2357-328-orangepi-failure.log`。


### 5.27 安全状态的最终继承

Linux `copy_seccomp` 在共同 `sighand->siglock` 下重新读取父线程过滤器、NNP 与 syscall-work，并由 `copy_process` 持锁完成线程组插入。Starry 现在在 `ProcessPolicyState` 持有任务上下文 PI mutex：`sys_seccomp` 的 strict/filter 安装及 TSYNC 与 `publish_clone` 共用此锁。clone 在资源准备、stage 以及可回滚的 cgroup/拓扑准备完成后，才调用 `Thread::inherit_security` 并发布 PID、附接任务和加入线程组；解锁后才安装已预留 pidfd、通知和首次 activate。

这条边界保证 TSYNC 先完成时 clone 继承新状态，clone 先发布时 TSYNC 的成员扫描包含子线程。锁顺序为进程安全更新锁、每线程快照存储锁（复制后释放）、PID 发布锁及线程组成员锁。新锁只用于可睡眠任务上下文，不模拟 raw rq 锁，也不改变 IRQ、迁移或调度交接。非 `CLONE_THREAD` 的孩子拥有独立进程锁，但最终快照仍在父进程锁下取得；单独共享 sighand 的另一进程不属于 TSYNC 线程组。

`SeccompStateStore::inherit` 在发布指针前执行可失败的 `try_reserve`。失败返回 ENOMEM，原快照、NNP 和 syscall-work 均不改变，发布回调不执行；外层既有创建令牌和身份事务撤销资源。成功后 immutable snapshot 仍保留到 Thread 析构，延续现有读者保护，不把 Arc 计数解释为 RCU 宽限期。这里没有改变普通 filter 更新的分配策略。

exec 的身份转交发生在同组其他线程退出后；退出请求只在 syscall 返回用户态前处理，已经开始的同步 TSYNC 会先完成。因此这次不在 exec 兄弟退出等待外增加安全更新锁，避免持锁等待正在取得同一锁的兄弟。该论证仅覆盖本次线程组更新与身份转交，不表示全部 exec 凭据语义已经核验。

真实 kernel axtest `clone_publication_refreshes_security_after_preparation` 在准备后更新父线程 BPF filter 与 NNP，验证发布时继承最新同一快照及 syscall-work；同时注入最终继承的分配失败，验证 ENOMEM 和未调用发布回调。最初旧继承顺序的确定性失败为 `clone published stale no_new_privs`，日志 `/tmp/pr2357-clone-security-red.log`。合并后的系统用例与静态检查结果见本节末；直接控制 syscall 并发交错仍未被普通系统用例覆盖。


补充分配次序回归将发布故意提前后，稳定失败于 `failed security inheritance must not publish a child`；恢复后同一用例和整个 x86_64 kernel axtest 183 项通过。日志 `/tmp/pr2357-clone-security-final-{red,green}.log`。增量 std 两包通过，日志 `/tmp/pr2357-clone-security-std.log`。合并后的 Starry/axbuild 定向 clippy 93/93 通过，日志 `/tmp/pr2357-merged-clippy.log`。四架构原始 seccomp 系统用例也通过，但没有据此宣称穷尽 clone/TSYNC 的 syscall 并发交错。

以下表项限定为新增的最终继承/同步边界；尚无直接 syscall 交错回归的入口使用“无法确认”，不能由 kernel 状态回归推导为完整 Linux 兼容。X/G 编号约定沿用第 4 节。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| clone / X56、G220 | [固定 copy_seccomp](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/fork.c#L1752) | 发布前最终继承父线程安全状态，失败不发布 | `sys_clone → CloneArgs::do_clone_in_cgroup → publish_clone → Thread`，父进程更新锁 | 无法确认 | x86 kernel 状态及失败次序红绿；直接 syscall 交错待验证 |
| clone3 / X435、G435 | [固定 copy_process](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/fork.c) | 参数处理后进入共同复制、发布协议 | `sys_clone3 → CloneArgs::do_clone_in_cgroup → publish_clone` | 无法确认 | 共用核心回归，clone3 直接交错待验证 |
| fork / X57 | [固定 copy_process](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/fork.c) | 新进程在可执行前继承过滤器 | `sys_fork → sys_clone → publish_clone` | 无法确认 | 四架构原 seccomp 的 libc fork 继承通过，未独立覆盖 X57 交错 |
| vfork / X58 | [固定 copy_process](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/fork.c) | 完成继承并激活后父线程才等待孩子 | `sys_vfork → sys_clone → publish_clone`，等待在锁外 | 无法确认 | x86_64 原 `syscall-test-vfork` 通过；该用例不验证过滤器交错 |
| seccomp(filter/TSYNC) / X317、G277 | [固定 seccomp_attach_filter](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/seccomp.c) | 同步更新与新线程最终复制/成员插入互斥 | `sys_seccomp → append_seccomp_filter → sync_seccomp_to_thread_group`，同一进程更新锁 | 无法确认 | kernel 晚期更新回归通过，直接并发 syscall 交错待验证 |
| seccomp(strict) / X317、G277 | [固定 seccomp_set_mode_strict](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/seccomp.c) | 参数检查后提交当前线程模式 | `sys_seccomp → install_seccomp_strict`，取得同一更新锁 | 无法确认 | 四架构原 `syscall-test-seccomp` 通过；独立锁交错未覆盖 |
| prctl(SET_SECCOMP) / X157、G167 | [固定 prctl_set_seccomp](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/seccomp.c) | legacy 入口复用模式安装，不支持 TSYNC 参数 | `sys_prctl → sys_seccomp(flags=0)` | 无法确认 | 四架构原 `syscall-test-seccomp` 通过；独立锁交错未覆盖 |


### 5.28 测试接口合并

合并 `origin/dev@481d6bdc4d` 的 shell-check 迁移时，保留 scheduler-latency-bench 的 UEFI/二进制启动设置，将成功匹配放入新的 `shell_check_steps`。`serial-rx` 的 AArch64 配置也补齐相同迁移，不保留已经移除的顶层 `success_regex`。超时、失败匹配和成功文本均未放宽。

`rust_qemu_features_for_run` 的精确列表断言继续覆盖 `task-cpu-lifecycle`，避免 dev 的两项列表排除本分支独立用例。NVMe 两分支已经修复同一 CID 复用误判，合并保留本分支在真实并发读回执上归一化 ID、改写第一块 CPU 缓冲区并验证第二块不变的单一回归，不重复增加同义用例。合并后的验证另行记录，不沿用合并前的绿色结果。


合并后增量 std 首轮 59 包中只有 axbuild 的精确清单断言失败：实际还包含 `serial-rx`，断言漏列。补齐后同一 axbuild 套件通过，其他 58 包沿用首轮通过结果；日志 `/tmp/pr2357-merged-std.log` 与 `/tmp/pr2357-merged-std-green.log`。Starry/axbuild 定向 clippy 为 93/93，通过后串行运行 kernel x86_64（183 项）、四架构 `syscall-test-seccomp`、x86_64 `syscall-test-vfork` 与 `test-thread-lifecycle-exec`、AArch64 `serial-rx`，8 项全部通过。日志统一为 `/tmp/pr2357-merged-<case>.log`。其中 serial-rx 实际执行新 shell-check 配置并完成原成功断言，不只是 TOML 解析通过。

`af0157b198` 的 CI 34486525334 已结束，唯一失败是普通 OrangePi 的 native-network-smoke；其 hardware-smoke 和其他作业通过。前轮普通 OrangePi 失败发生在 board-2，日志停在 Submit 结构体输出中途并含 NUL；更早 board-1 曾完成 NPU/脚本/cat 后停滞。两者尚未确定共同根因，不用更换板卡或重跑通过关闭问题。临时诊断配置位于仓库外，保留原超时与判定，仅增加线程状态读取；结果仍需实际运行核实。


临时线程状态诊断在此前失败过的 `OrangePi-5-Plus-1` 上完成了 NPU、grep 和整个硬件检查，日志 `/tmp/pr2357-orangepi-task-diagnostic.log`。租约 `52ed0bf7-7400-4c12-8faa-3ce7af008182` 的运行命令正常结束；用例在延迟状态采样前结束，没有捕获挂起。成功日志在启动阶段同样含 NUL，因此不能仅凭 NUL 推断挂起根因。该结果只证明本次诊断运行通过，不关闭历史失败。


### 5.29 vfork 等待的线程归属

重新对照固定 Linux `wait_for_vfork_done`、`mm_release` 与 `signal_pending_state` 后，确认等待不能只响应 Starry 内部退出请求：SIGKILL 必须能在孩子仍未释放 MM 时结束父线程的等待，并断开孩子的完成指针；只有正常完成才报告 `PTRACE_EVENT_VFORK_DONE`。现有 C 用例增加独立观察者和两条管道，由观察者先等待父线程被 SIGKILL 回收，之后才放行孩子。10 秒 alarm 仅负责错误实现的诊断与清理，交错由管道握手确定。

vfork 完成状态现在属于 `ThreadLifecycle`，从 `ProcessWaitState` 移除；clone 在发布前通过 `Thread::prepare_vfork_done` 做可失败分配，父线程持有激活后的 `UserTaskRef` 等待指定孩子。`do_exit` 对每个线程通知，exec 通知其当前线程。父线程被杀死时，在该孩子的 IRQ 锁内取走完成对象，在解锁后释放；普通信号仍重新等待。legacy clone 不再沿用 clone3 的线程 exit-signal 限制，也不再拒绝 Linux 支持的 `CLONE_THREAD|CLONE_VFORK`；clone3 自己的参数检查保留。

musl 的 `clone()` 包装会在进入内核前拒绝 `CLONE_THREAD`，不能用它的 EINVAL 判断 Starry 是否进入了完成等待。测试对此组合使用强制内联的四架构原始 clone ABI，并依赖 VFORK 保证父栈在孩子单线程 `SYS_exit` 前不被父线程继续使用。最终直接 ABI 回归在“只对最后线程通知”的错误次序上稳定触发 watchdog，恢复每线程通知后 x86_64 同一用例通过，日志 `/tmp/pr2357-vfork-thread-owner-raw-{red,green}.log`。SIGKILL 原实现失败、修复后通过的日志为 `/tmp/pr2357-vfork-killable-{red,green}.log`。宿主 Linux 的相同两项直接 ABI 检查均通过，日志 `/tmp/pr2357-vfork-waits-linux.log`；它只是额外 ABI 对照，不是本地 v7.1 PREEMPT_RT 运行验证。

本轮还发现必须继续修正的边界，不能把上面的成功扩大为完整 vfork/MM 对齐：Linux 的 `mm_release` 在 `exit_mm` 内通知 vfork，早于 `exit_shm/exit_files`；当前通知仍在现有退出清理尾部，exec 也仍在现有清理尾部通知。更重要的是，原 SHM 用例要求共享 MM 下孩子退出后 `shm_nattch == 1`，在 Linux 上不成立：宿主的 wait 前后均为 2，固定 Linux `shm_vm_ops` 的 open/close 也把附接绑定于 VMA，而非创建映射的 PID。Starry `clear_proc_shm` 按 PID 退出主动 unmap，会移除仍由共享 MM 使用的映射。原 SHM 通过记录仅说明满足旧测试，撤回其 Linux 等价性解释；需要修改实际 MM/VMA 所有权后，用正确的共享映射存续和分离行为替换旧预期，不能只改断言。

本表限定当前已核实范围；4cacc4874b 的四架构等待回归已完成，后续 MM 改动和尚未直接观测的 ptrace 事件仍保留缺口。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| clone(VFORK, SIGKILL) / X56、G220 | [固定 wait_for_vfork_done](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/fork.c#L1430) | 父线程可在孩子完成前被 SIGKILL 终止；不报告 VFORK_DONE | `sys_clone → Thread::wait_vfork_done → pending(SIGKILL)` | 无法确认 | x86_64 红绿、四架构 QEMU 和宿主直接 ABI 通过；ptrace 事件仍缺直接观测 |
| clone(THREAD,VFORK) / X56、G220 | [固定 kernel_clone](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/fork.c) | 指定子线程释放 MM 后通知父线程 | `sys_clone → 子线程 ThreadLifecycle::vfork_done → do_exit` | 部分正确 | 每线程归属 x86 原始 ABI 红绿；通知仍晚于 Linux MM-release 阶段，见本节缺口 |
| clone3(VFORK) / X435、G435 | [固定 clone3_args_valid](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/fork.c#L2962) | clone3 保留自己的 flag/exit-signal 校验，再进入相同等待协议 | `sys_clone3 → Clone3Args::try_from → CloneArgs → Thread` | 无法确认 | 参数校验保持，新的等待场景尚无 clone3 直接回归 |
| vfork / X58；G 以 clone 实现 | [固定 mm_release](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/fork.c#L1463) | 等待子线程 MM release，不等同进程资源全部清理 | `sys_vfork → sys_clone → Thread` | 部分正确 | 普通共享与阻塞通过；当前 exit/exec 通知位置仍待调整 |
| shmat / X30、G196 | [固定 shm_vm_ops](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/ipc/shm.c#L685) | 共享 MM 中的 VMA 不随映射创建者 PID 退出而消失 | `sys_shmat → ShmInner PID 索引 → do_exit::clear_proc_shm` | 不正确 | Linux wait 前后 nattch=2，Starry 旧测试要求 1；`/tmp/pr2357-vfork-shm-linux.log` |
| shmdt / X67、G197 | [固定 ksys_shmdt](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/ipc/shm.c) | 通过当前 MM 的 VMA 分离映射，不要求创建该映射的 PID 相同 | `sys_shmdt → get_shmid_by_vaddr(owner PID) → unmap` | 不正确 | `shm.rs:897` 按调用者 PID 查找，无法分离另一共享 MM 进程建立的映射；直接回归待补 |
| shmctl(IPC_STAT) / X31、G195 | [固定 shmctl](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/ipc/shm.c) | nattch 反映存活 VMA 附接 | `sys_shmctl → ShmInner::attach_count` | 不正确 | 原 C 用例在 Linux 观察到 wait 前后均为 2，Starry 退出按 PID 清理后降为 1 |

普通 OrangePi 最新失败日志为 `/tmp/pr2357-af015-orangepi-failure.log`：hardware-smoke 在 board-2 通过，network-smoke 在 board-3 的 init.sh 连续 touch 阶段之后停止，尚未出现交互 shell；DHCP 与 ARP 工作继续。不能再把该轮失败定位为 NPU 或已经开始的 iperf 工作负载。


`Thread::prepare_vfork_done` 的分配故障已纳入现有 kernel 创建回滚用例：错误的 `Arc::new` 路径稳定失败于 `vfork completion allocation failure must return ENOMEM`，恢复可失败创建后 x86_64 kernel 183/183 通过；失败时完成槽保持空，并可在同一未发布线程上重新准备。日志 `/tmp/pr2357-vfork-allocation-{red,green}.log`。`4cacc4874b` 已完成四架构 `syscall-test-vfork`、x86_64 `test-nix-clone-parent`、增量 `cargo xtask test --since af0157b198` 和定向 `cargo xtask clippy --package starry-kernel` 92/92。日志为 `/tmp/pr2357-vfork-final-{riscv64,aarch64,loongarch64}.log`、`/tmp/pr2357-vfork-clone3-x86_64.log`、`/tmp/pr2357-vfork-std.log`、`/tmp/pr2357-vfork-clippy.log`；这些检查使用新代码，没有沿用旧 head 的结果。


### 5.30 初始名称分配回滚

`CloneArgs::do_clone_in_cgroup` 原先在进入 `UserThreadOptions` 前执行 `String::from`，这一步不能返回 `ENOMEM`，因此未被后续可失败 builder 覆盖。`UserThreadOptions::new` 改为借用名称并返回 `Result`，在身份发布前通过 `task::allocation::try_string` 获取所有权；clone 使用既有 `map_task_creation_error` 转换错误，init 和内核测试迁移到相同入口。`prepare_task_name` 复用同一字符串分配和 `try_arc`，删除重复分配实现。这里对照 Linux `copy_process` 的失败撤销和 `wake_up_new_task` 前不可运行要求；不声称 Linux 的内嵌 `comm` 字段需要同样的堆布局。

现有 `task_name_allocation_failure_preserves_snapshot` 增加初始名称故障与恢复检查，错误的 `String::from` 路径稳定失败于 `initial thread name allocation failure must return ENOMEM`；修复后同一 x86_64 QEMU kernel 测试通过，整组 183/183。`unpublished_extension_allocation_releases_process` 同时覆盖名称准备阶段，验证已有 Thread、进程和 PID 预留随提前返回释放，入口不运行。日志为 `/tmp/pr2357-initial-name-{red,green}.log`。真实 clone 路径由 x86_64 `syscall-test-vfork` 通过验证，日志 `/tmp/pr2357-initial-name-clone.log`；定向 `cargo xtask clippy --package starry-kernel` 已完成四架构 92/92，日志 `/tmp/pr2357-initial-name-clippy.log`。提交前的增量 std 未选中未提交修改，因此不计为验证；提交后以 `4cacc4874b` 为基线重新执行，实际选中 `starry-kernel` 且全部通过，日志 `/tmp/pr2357-initial-name-std-committed.log`。

本节只修复初始名称这一分配缺口。clone 的 pidfd `Arc::new` 及其他进程资源分配仍需要逐项审核，不能将此用例扩大为整个 copy_process 的全部分配失败证明。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| clone(名称分配失败) / X56、G220 | [固定 copy_process](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/fork.c#L2089) | 创建资源不足时在发布和首次运行前撤销并返回 ENOMEM | `sys_clone → CloneArgs::do_clone_in_cgroup → UserThreadOptions::new → try_string → map_task_creation_error` | 无法确认 | 名称故障与进程回收的真实 kernel 红绿、普通 clone 用例通过；尚无从用户 syscall 注入该精确分配故障的证据 |
| clone3(名称分配失败) / X435、G435 | [固定 kernel_clone](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/fork.c) | 参数验证后遵循相同创建回滚 | `sys_clone3 → CloneArgs::do_clone_in_cgroup → UserThreadOptions::new` | 无法确认 | 共用已验证的名称准备路径；未对 clone3 入口注入名称故障 |
| fork(名称分配失败) / X57；G 无独立入口 | [固定 fork](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/fork.c) | 创建资源失败时不发布子进程 | `sys_fork → sys_clone → UserThreadOptions::new` | 无法确认 | 共用已验证的名称准备路径；未对 fork 入口注入名称故障 |
| vfork(名称分配失败) / X58；G 以 clone 实现 | [固定 vfork](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/fork.c) | 创建失败时不进入等待孩子完成的阶段 | `sys_vfork → sys_clone → UserThreadOptions::new` | 无法确认 | x86_64 普通等待路径通过；精确失败仍由 kernel 创建用例覆盖 |

SysV SHM 后续回归已先在宿主 Linux 验证：同一共享 MM 中，孩子附接、退出后，父进程在 wait 前后均观察到 nattch=2，仍能读取孩子的映射，并能对孩子创建的地址执行 shmdt，随后 nattch=1。临时程序 `/tmp/pr2357-shm-mm-regression.c`，结果 `/tmp/pr2357-shm-mm-regression-linux.log`。该证据补充第 5.29 节的反例，不代表 Starry 已修复。VMA 事务核验还确认 `publish_vma_metadata_successor` 仅覆盖部分元数据路径，map/unmap/mprotect/mremap/fork/clear 并不统一经过它；后续逻辑附接提交必须同时覆盖这些路径及失败回滚，不能仅绑定该函数或 backend Arc 的析构。


### 5.31 致命信号发布

固定 Linux `complete_signal` 在找到可接收信号的线程后，对默认、非 coredump 且不受 ptrace 延迟的致命信号提交 `SIGNAL_GROUP_EXIT` 和原始退出码，再向所有线程发布 SIGKILL 并唤醒。`get_signal` 优先处理该退出决定，`do_group_exit` 保留已经提交的退出码。这解释了为何 vfork 父线程不仅能被 SIGKILL 终止，也应能在孩子完成前被默认 SIGTERM 或实时信号终止。原实现仅排队原始信号，vfork 的 killable 等待看不到 SIGKILL，必须等孩子完成才能继续处理信号。

`GroupExit` 现在保存同一线程组不可撤销的退出决定，由 `Process` 和 `ProcessSignalManager` 共享；原 `ThreadGroup::group_exited` 被移除，`last_exit_code` 只记录尚未形成线程组退出决定时的线程退出值。`ProcessData::thread_group_update` 从原 seccomp 门禁扩展为信号、退出和 clone 最终发布的共同任务上下文 PI 门禁。处置、pending 与 `GroupExit` 的短状态锁不执行调度通知；`queue_thread_signal`、`publish_process_signal` 在释放共同门禁后释放停止状态并显式唤醒任务。组件内的 sigwait waker 已在处置锁外调用，但仍处于外层共同门禁期间，其回调上下文仍需单独收尾，不能笼统认定所有通知均已移至门禁外。clone 在该门禁下检查父线程的 SIGKILL，依照 `copy_process` 在 PID 发布前返回 EINTR。`exit_group` 通过 `ThreadSignalManager::begin_group_exit` 在同一处置锁内提交共享退出状态和所有其他线程的 SIGKILL，释放共同门禁后才通知。该事务先于 `Thread::begin_exit` 的 PF_EXITING 声明，返回最先提交的 wait status 供后续清理使用，对齐 `do_group_exit → do_exit`。原先先提交状态、解锁后逐线程发送的窗口以及 `Process::start_group_exit` 被删除；进程退出也不再借用 exec 的 `zap_thread`。

`publish_with_targets` 在取得处置锁后重新核验保留的线程集合；新接收者登记和 exec TID 更名使用同一处置锁，防止锁外快照漏掉新成员。已经作出的退出决定会为后来准备的接收者设置 SIGKILL，clone 的最终发布检查仍会拒绝它。`PF_EXITING` 对应的单次退出声明移入 `ThreadSignalManager`，`Thread::begin_exit` 委托这一声明，不再维护第二份标志。发送资格排除已开始退出的线程；退出期间的信号检查停止重入清理。被屏蔽、带用户处理器、coredump 或由 OS 要求延迟致命处理的信号不走提前线程组终止分支。实时信号此前落入错误的默认 Ignore 分支，现按 Linux 的 ignore/stop mask 改为默认 Terminate。

信号队列的准备和发布也必须分开。`PreparedSignalInfo` 在进入处置锁前持有标准信号的 Box 或实时信号的单个链表节点；锁内只插入或拼接，重复或未使用的预备对象在解锁后释放。实时 FIFO 使用 Rust `LinkedList` 传递已经分配的节点，避免 `VecDeque::push_back` 在处置锁内扩容。现有锁内堆操作测试已扩展到实时信号，原扩容路径稳定报告 1 次锁内堆操作，修复后为 0。本项证据覆盖处置锁，不替代全部 pending 释放上下文与 RT 锁分类的最终审核。

线程定向通知通过 `Thread::scheduler_id → ThreadHandle::lookup → UserTaskRef::try_from_scheduler` 保留原调度代际，不再重新查询可能复用的数字 TID。组通知在解析成员后额外复核进程身份，避免成员快照过期后唤醒无关进程。这些变更复用已有身份与句柄协议，没有新增弱引用登记表；尚未取得强制 TID 复用交错的直接红绿证据。

新增共享状态不能引入新的不可恢复创建点。`Process::allocate` 对 `GroupExit` 和 `Process` 均使用可失败的 `task::allocation::try_arc`；`prepare_fork` 返回 `Result`，clone 在发布前传播错误，bootstrap 和测试入口明确处理初始化失败。旧的、实际只用于 bootstrap 的通用 `new(parent)` 分支收敛为 `new_bootstrap`，删除失效的 `ProcessBuilder` 注释。已有不可见性用例扩展覆盖两次分配失败、ENOMEM 和父进程无已发布孩子。

本轮已取得的红绿证据按实际风险记录，尚在运行的矩阵不视为通过。

| 风险 | 红证据 | 绿证据 |
| --- | --- | --- |
| 默认 SIGTERM 的 vfork 父线程不能被及时终止 | `/tmp/pr2357-vfork-sigterm-red.log`，watchdog 触发 | `/tmp/pr2357-vfork-sigterm-green.log`，同一 x86_64 用例通过且 wait 状态仍为 SIGTERM |
| 实时信号默认被忽略 | `/tmp/pr2357-vfork-realtime-red.log`，同一父线程等待失败 | `/tmp/pr2357-vfork-realtime-green.log`，同一 x86_64 用例通过 |
| 处置锁内分配 | 标准信号 `/tmp/pr2357-fatal-publication-std-red.log`；实时信号 `/tmp/pr2357-fatal-realtime-allocation-red-confirmed.log` | `/tmp/pr2357-fatal-publication-std-green.log`，锁内堆操作为 0 |
| 选中已进入退出的线程 | `/tmp/pr2357-fatal-receiver-red.log`，错误选中 TID 1 | `/tmp/pr2357-fatal-receiver-green.log`，排除该接收者并覆盖屏蔽、处理器和 coredump 条件 |
| 排队信号多读取不存在的 sigsetsize | `/tmp/pr2357-queue-arity-{sigqueueinfo,tgsigqueueinfo}-red.log`，旧实现对显式零的未使用参数返回 EINVAL | `/tmp/pr2357-queue-arity-<arch>-<case>-green.log`，同一首项断言在四架构通过；FIFO 红绿另见 signal-routes/queued-abi 日志 |
| exit_group 解锁时 peer 尚未持有 SIGKILL | `/tmp/pr2357-exit-group-publication-red.log`，恢复只发布决定的旧边界后 peer pending 断言失败 | `/tmp/pr2357-exit-group-publication-green.log`，同一回归及两包 std 通过；最后版本 `/tmp/pr2357-group-serial-std.log` 再次通过 |
| 进程准备分配不能返回 ENOMEM | `/tmp/pr2357-process-preparation-allocation-red.log` | `/tmp/pr2357-process-preparation-allocation-green.log`，kernel 183/183 |

宿主 Linux 的同一 SIGKILL、SIGTERM、实时信号和 THREAD/VFORK 场景均通过，日志 `/tmp/pr2357-vfork-realtime-linux.log`；这仍是额外 ABI 对照，不能当作固定 v7.1 PREEMPT_RT 构建验证。两包定向 clippy 已完成 93/93，增量 std、四架构 vfork 和另外 9 个 x86_64 系统用例全部通过，日志为 `/tmp/pr2357-fatal-*`。其中 `syscall-test-uid-gid-re-setters` 返回 0 fail，但该次成功不证明历史超时根因已定位。扩展的六种信号发送入口和实时 FIFO 已通过四架构 vfork 用例，日志 `/tmp/pr2357-group-final-<arch>-syscall-test-vfork.log`；最后退出顺序调整后的 x86_64 额外复跑也通过，日志 `/tmp/pr2357-group-restored-x86_64-syscall-test-vfork.log`。最后版本的 x86_64 kernel 为 183/183，日志 `/tmp/pr2357-group-serial-kernel.log`；增量 std 通过。一次定向 clippy 被并行 QEMU 构建写入的 `bitmaps@3.2.1` future-incompatibility 报告拦下；停止并行构建后，保持原门禁的串行复验两包 93/93 全部通过，日志 `/tmp/pr2357-group-serial-clippy.log`。已推送的 `05447c0700` 在 CI run `34498542687` 完成 34 项、全部成功，不能将其结果用于本节尚未推送的代码，也不能据此认定历史 OrangePi 停滞根因已经修复。

`syscall-test-sigqueueinfo` 和 `syscall-test-tgsigqueueinfo` 原先安装到 `starry-known-fail`，未进入分组成功条件。现已迁回 `starry-test-suit`；未修改既有断言，x86_64 最初分别取得 8/8、14/14，之后补充 Linux 不使用的第四/第五参数必须被忽略的确定性 ABI 断言，宿主 Linux 均通过。旧实现确定性返回 EINVAL，恢复修复后同一首项断言及整个用例在四架构分别 9/9、15/15 通过，日志 `/tmp/pr2357-queue-arity-<arch>-<case>-green.log`。

下表限定本节直接调整的发送、创建和退出语义。X 为 x86_64，G 为 riscv64、aarch64、loongarch64 的通用编号。没有直接入口回归的分支保留为无法确认；本节不把共享状态的 Arc 当作 MM、栈或 RCU 宽限期证明。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| kill(默认致命信号) / X62、G129 | [固定 complete_signal](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/signal.c#L958) | 先提交线程组终止，再结束 killable 等待；保留原始信号退出码 | `sys_kill → send_signal_to_process_data → publish_process_signal → ProcessSignalManager → GroupExit → wake_exiting_signal_group` | 无法确认 | 四架构 SIGKILL/SIGTERM/实时信号直接回归及宿主对照通过；完整权限和停止竞争未覆盖 |
| tkill / X200、G130 | [固定 do_tkill](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/signal.c) | 线程定向信号在合格接收者上仍可终止整个线程组 | `sys_tkill → send_signal_to_task → queue_thread_signal` | 无法确认 | 四架构 vfork 父线程的 tkill 致命投递通过；完整凭据和线程组竞争未覆盖 |
| tgkill / X234、G131 | [固定 do_tkill](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/signal.c) | 先检查指定线程组，再执行线程定向投递 | `sys_tgkill → send_signal_to_task → expected_process 校验 → queue_thread_signal` | 无法确认 | 四架构 vfork 父线程的 tgkill 致命投递通过；完整权限/错误优先级仍需核验 |
| rt_sigqueueinfo / X129、G138 | [固定 do_rt_sigqueueinfo](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/signal.c) | 实时信号保留每个排队实例；默认动作遵守相同致命判断 | `sys_rt_sigqueueinfo → send_signal_to_process_data → PreparedSignalInfo → pending FIFO` | 部分正确 | 四架构致命投递、FIFO 和三参数 ABI 9/9 通过；`make_queue_signal_info` 的 signo=0 复制顺序、si_code 原点判断和入口权限检查仍有实现缺口 |
| rt_tgsigqueueinfo / X297、G240 | [固定 do_rt_tgsigqueueinfo](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/signal.c) | 线程组校验后向指定线程排队 | `sys_rt_tgsigqueueinfo → send_signal_to_task → queue_thread_signal → pending FIFO` | 部分正确 | 四架构致命投递、FIFO 和四参数 ABI 15/15 通过；复制、参数、身份匹配与权限检查顺序仍需按 `do_rt_tgsigqueueinfo → do_send_specific` 修复 |
| pidfd_send_signal / X424、G424 | [固定 pidfd_send_signal](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/signal.c) | 使用稳定身份选择进程或线程后执行相同投递规则 | `sys_pidfd_send_signal → 已解析 PID generation → process/task 投递` | 无法确认 | 四架构 vfork 致命投递通过；x86_64 原 pidfd 发送用例通过；其余 flag/权限分支未全覆盖 |
| clone(最终发布) / X56、G220 | [固定 copy_process](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/fork.c#L2448) | 父线程已有致命信号时，在发布前撤销并返回 EINTR | `CloneArgs::do_clone_in_cgroup → publish_clone → thread_group_update → pending(SIGKILL)` | 无法确认 | 真实 kernel 中最终发布回调禁止/EINTR 的确定性红绿通过；尚非 clone syscall 入口级并发注入 |
| clone3(最终发布) / X435、G435 | [固定 kernel_clone](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/fork.c) | clone3 参数验证后进入同一不可运行准备和最后发布检查 | `sys_clone3 → CloneArgs::do_clone_in_cgroup → publish_clone` | 无法确认 | 共用发布实现；该新分支尚无 clone3 入口回归 |
| fork(进程准备) / X57；G 无独立入口 | [固定 copy_process](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/fork.c#L2089) | 分配失败不发布孩子并返回 ENOMEM | `sys_fork → sys_clone → Process::prepare_fork → try_arc` | 无法确认 | 两个新分配点的真实 kernel 红绿通过，入口级注入未完成 |
| vfork / X58；G 以 clone 实现 | [固定 wait_for_vfork_done](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/fork.c#L1430) | 等待可被终止；正常完成关联 MM release | `sys_vfork → sys_clone → Thread::wait_vfork_done` | 部分正确 | 默认致命信号等待已修；MM 通知仍在旧清理尾部，缺口见第 5.29 节 |
| exit / X60、G93 | [固定 synchronize_group_exit](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/exit.c#L867) | 每线程退出只声明一次，最后线程保留已提交的线程组状态 | `sys_exit → do_exit → Thread::begin_exit → Process::exit_thread → GroupExit` | 部分正确 | vfork 子线程退出与 kernel 回滚通过；MM/SHM 退出阶段仍待修正 |
| exit_group / X231、G94 | [固定 do_group_exit](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/exit.c#L1098) | 一次线程组退出决定不能被后续 SIGKILL 或线程退出覆盖 | `sys_exit_group → do_exit → ThreadSignalManager::begin_group_exit → GroupExit/peer SIGKILL → 解锁后通知 → Thread::begin_exit` | 部分正确 | peer 发布确定性红绿、首个退出码保持、x86_64 SIGCHLD 退出码用例和 kernel 183/183 通过；MM/SHM 阶段仍需修复 |

合入前仍需取得调度与锁领域审核。现有处置锁和 pending 锁的 raw 分类必须按全部调用上下文继续核验；本轮共同 PI 门禁的串行证明不等于所有锁类型已经完成 RT 收尾。

clone 最终发布的 EINTR 分支已扩展进既有 `clone_publication_refreshes_security_after_preparation`：省略致命信号检查时，测试稳定进入禁止的发布回调并失败；恢复检查后返回 EINTR，kernel 183/183。日志 `/tmp/pr2357-clone-fatal-publication-{red,green}.log`。`publish_clone` 的命名覆盖最终信号检查、安全继承和身份发布这一个提交边界，不再只指 seccomp。

### 5.32 SysV SHM 的 VMA 生命周期

该项正在实施，尚未宣称修复。`CLONE_VM|CLONE_VFORK` 的孩子建立的附接必须继续属于共享 MM，父进程可以读取该映射并通过原地址执行 shmdt。新的 `test_clone_vfork_shared_shm` 替换了旧的 PID 退出清理预期，并删除对 nattch 的低 16 位掩码。当前 Starry 的确定性红证据为 `/tmp/pr2357-shm-shared-mm-red.log`：wait 前后计数均不符合 2，父进程 shmdt 失败；退出与自身附接清理本身成功。这不是单纯改变断言，而是明确尚未实现的所有权要求。

固定 Linux `ipc/shm.c` 的 `shm_open/__shm_close` 随 VMA 建立和关闭维护附接；`mm/vma.c::can_merge_remove_vma` 禁止通过合并移除带 close 操作的 VMA。宿主对照 `/tmp/pr2357-shm-vma-count-linux.log` 的实际计数为：初次附接 1，mprotect 分裂后 3，恢复权限后仍为 3，munmap 中间页后 2，fork 孩子存活时 4，孩子退出后 2。该程序在开始时设置 IPC_RMID，所有映射最终分离；不把这个宿主结果当作固定 RT 构建证据。

只把 `ShmInner::va_range` 和 `ShmManager::pid_shmid_vaddr` 的键改成 MM ID 不能覆盖 VMA 分裂、fork、部分 munmap 和 exec。把计数放入 SharedMemoryObject 或普通 Arc 的 Drop 也不正确，因为旧 VMA 根、PTE 槽和 TLB 回执可能保留物理资源。实现应复用现有 VMA 事务，令逻辑附接跟随已提交的 VMA 身份，物理页仍由 SharedMemoryObject、MappingOperation 和 MappingSlot 持有；不增加通用插件或另一个独立的内存对象登记体系。

`publish_mutation_classified` 已区分未发布失败和已发布但 TLB 待确认。map/unmap/mprotect/mremap 都保留自己的 preimage；fork 为子 MM 提交独立回执，失败走 `reset_uninstalled_for_loader`。需要保留逻辑 VMA 前像，但不能只按最终根或附接净增量生成生命周期回调：固定 Linux `mm/vma.c:1427,1448` 在中段 munmap 前分别执行两次 `__split_vma`，`:543` 调用新片段的 open，随后移除中间 VMA 才执行 close。最终数量从 1 变成 2，实际却有两次 open 和一次 close。后续实现须记录操作级分裂、建立和关闭阶段，分别处理已经发布的分裂、未发布失败和 PublishedPendingTlb；不能只挂在 `publish_vma_metadata_successor`，也不能漏掉不经过普通回执的未发布 MM 清理。宿主探针 `/tmp/pr2357-shm-split-times.c` 和 `.log` 同时观察到中段 munmap 后 nattch 1→2、atime 更新及 dtime 从 0 更新；最初对精确秒边界的额外假设失败，改为核验实际 open/close 引起的前后变化，不把该宿主探针当作 Starry 或 RT 构建验证。

锁顺序需要同时迁移：当前 `sys_shmat` 持 ShmInner 锁调用 `aspace.map_outcome`；VMA 关闭回到段记账后会形成反向顺序。映射准备必须先取得稳定段/物理对象的预留，释放 IPC 和段锁，再进入 MM 事务。IPC_RMID 竞争由这项预留保护，失败撤销预留，不在 MM 锁内重新走原来的 SHM_MANAGER→ShmInner→aspace 链。正常 shmdt 从当前 MM 的 VMA 来源和原附接地址选择片段，不再依赖创建映射的 PID。

MM 用户所有权结束时要关闭逻辑附接，不能等 lazy active-MM 和旧 PTE 所有者最终析构才更新 nattch；同时必须串行化仍在进行的 VMA 修改，并禁止已关闭映射重新发布附接。现有 `MmHandle::user_refs` 表示进程拥有者，RuntimeMmOwner 还持有每任务 MmPin，不能直接把其中一个计数当成 Linux mm_users。具体退出准入、内核读者、操作 PID 和任务上下文释放协议需要一并证明，然后才能移动 exit/exec 的 vfork MM-release 通知。

验收覆盖共享 MM、普通 fork、VMA 分裂和不合并、部分 munmap、mremap、exec、IPC_RMID 最后附接、预发布失败和 PublishedPendingTlb。保留旧 VMA 元数据快照或 active-MM 时，也必须分别验证逻辑计数与物理资源寿命。所有计数变更和关闭只能发生一次，释放路径不能在 raw 锁或 IRQ 上下文取得可睡眠锁。

### 5.33 文件描述符准备

Linux `pidfd_prepare` 先经 `get_unused_fd` 预留编号，再执行可失败的 `pidfs_alloc_file`；文件创建失败时释放预留，后续 clone 失败则分别撤销编号和文件引用。Starry 的 `prepare_file_like` 改为先预留，再在锁外调用可失败的文件工厂。clone 的 pidfd 使用 `Arc::try_new`，失败返回 ENOMEM。`PreparedFileDescriptor` 区分 Reserved、Ready 和 Installed，确保创建失败、copyout 失败和取消均撤销原表中的预留，安装后的析构不会删除已经被另一个线程关闭并复用的 FD。SCM_RIGHTS 和 socketpair 迁移到同一入口，原有 copyout-before-install 边界保持不变。

`FileTable` 原先在 raw 写锁内通过 `BTreeSet` 分配和释放预留节点。现在在已有 `FlattenObjects` 的内联槽和位图中保存 `FileSlot::Reserved` 或 `Installed`，不再维护第二份预留集合。查找、关闭和枚举只观察 Installed；dup2/dup3 对 Reserved 仍返回 EBUSY，复制 FD 表只复制已安装文件。文件析构始终在本次准备事务释放 raw 表锁之后执行。本节不证明其余所有 FD 操作都已排除锁内分配：表复制的 generation Arc、批量关闭收集等路径仍需继续核验。

确定性回归 `descriptor_reservation_precedes_fallible_file_creation` 在创建入口观察预留、确认表写锁可取得，再注入 ENOMEM，检查编号释放和立即复用，并检查 FD 耗尽时不调用文件工厂。旧顺序稳定失败于预留数量 0 而非 1，日志 `/tmp/pr2357-fd-reservation-red.log`；新顺序同一回归及 x86_64 kernel 184/184 通过，日志 `/tmp/pr2357-fd-reservation-green.log`。这项 kernel 证据不能扩大为用户 clone 的精确分配故障注入。随后 `cargo xtask clippy --package starry-kernel` 完成四架构 92/92，日志 `/tmp/pr2357-fd-reservation-clippy.log`；没有执行全工作区 clippy。

其余兼容缺口分别记录，不能因本次准备事务通过而视作已解决：`sys_socketpair` 尚未按照 Linux `__sys_socketpair` 在创建 socket 前预留两个 FD 并完成结果复制；FD 分配仍用现有 count 近似 RLIMIT_NOFILE，而 Linux `alloc_fd` 比较候选编号和上限。这两项需要独立的错误顺序回归和实现收尾。第 5.32 节的共享 MM / SysV SHM 生命周期也仍未实现，本地尚未提交的共享 MM 回归目前为红。

当前远端基线 `8e44bbc49e` 的 CI run `34517679621` 已结束：28 项成功、4 项取消、2 项失败。Starry OrangePi-5-Plus-1 hardware-smoke 停滞根因尚未定位；同板诊断通过不能算修复。另一个 Axvisor robot 项仅因 FPS 27.78 低于 28 失败，按当前任务约定暂停性能调查，不修改阈值。取消的 Starry QEMU 和 SG2002 项不能视为通过。

本节按实际改变的文件准备调用链区分证据范围。X 表示 x86_64，G 表示 aarch64、riscv64 和 loongarch64 的 asm-generic 编号；表中结论不代表对应系统调用的全部参数已经完成兼容。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| clone(CLONE_PIDFD 准备) / X56、G220 | [v7.1 pidfd_prepare](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/fork.c#L1865) | 先预留 FD，再创建 pidfd 文件；失败撤销且不运行孩子 | `sys_clone → do_clone_in_cgroup → prepare_file_like → PreparedFileDescriptor`，文件表由进程共享 | 无法确认 | kernel 预留顺序和 ENOMEM 红绿；尚无从用户 clone 注入实际 pidfd 分配失败的证据 |
| clone3(CLONE_PIDFD 准备) / X435、G435 | [v7.1 pidfd_prepare](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/fork.c#L1865) | 参数检查后按相同预留和创建顺序准备 pidfd | `sys_clone3 → do_clone_in_cgroup → prepare_file_like` | 无法确认 | 共用 kernel 准备事务；未注入 clone3 的实际 pidfd 分配失败 |
| socketpair(准备与结果复制) / X53、G199 | [v7.1 __sys_socketpair](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/net/socket.c#L1828) | 两个 FD 预留和结果复制先于 socket 创建，失败释放预留 | `sys_socketpair → prepare_file_like → PreparedFileDescriptor::install` | 部分正确 | copyout 前不安装、失败撤销；实现仍先创建 socket，错误优先级待修复 |
| recvmsg(SCM_RIGHTS) / X47、G212 | [v7.1 scm_detach_fds](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/net/core/scm.c#L354) | 逐 FD 预留、复制编号、安装；部分结果按 cmsg 边界处理 | `sys_recvmsg → recv_impl → CMsgBuilder::push_rights → prepare_file_like → install` | 无法确认 | 工厂复用现有文件 Arc；本轮 x86_64 `test-unix-scm-rights` 已通过，其他三架构该用例未复跑 |
| recvmmsg(SCM_RIGHTS) / X299、G243 | [v7.1 do_recvmmsg](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/net/socket.c) | 每条消息独立提交已成功接收的描述符 | `sys_recvmmsg → recv_impl → CMsgBuilder::push_rights` | 无法确认 | 共用 FD 事务，尚无批量消息携带 SCM_RIGHTS 的本轮回归 |

`CLONE_PIDFD` 的 NULL 输出指针也必须进入 pidfd 准备和 copyout，而不是被当成“不请求 pidfd”。现删除 pidfd 准备分支中的 `pidfd != 0`，并删除 clone 开始时对 pidfd/parent_tid 的提前 `prepare_user_write`。pidfd 的错误指针在预留之后、身份发布之前由 `vm_write` 返回 EFAULT；FD 耗尽则先返回 EMFILE。`CLONE_PARENT_SETTID` 继续在孩子发布之后写入，写失败不撤销孩子，对照固定 Linux `copy_process` 的 `put_user(pidfd, args->pidfd)`（kernel/fork.c:2310）与 `kernel_clone` 的 `put_user(nr, args->parent_tid)`（:2738）。两类 copyout 不能合并为一项创建前预检查。

扩展的 `syscall-test-clone-tls` 取得分层红证据：原实现对 NULL pidfd 实际创建孩子（`/tmp/pr2357-null-pidfd-red.log`）；只修补提前 NULL 检查的中间实现仍在 FD 耗尽时错误返回 EFAULT（`/tmp/pr2357-pidfd-error-order-red.log`）；无效 parent_tid 同样被提前拒绝，孩子没有创建（`/tmp/pr2357-clone-copyout-red.log`）。最后同一完整 C 用例在宿主 Linux 40 pass、0 fail，日志 `/tmp/pr2357-clone-copyout-linux.log`。中间版本的三个架构通过记录不代表最后的错误顺序已验证；最终四架构回归均为 40 pass、0 fail，使用独立的 `/tmp/pr2357-fd-copyout-green-<arch>-syscall-test-clone-tls.log`。x86_64 的 `test-unix-scm-rights`、`bugfix-usercopy-socket-results` 和 `test-clone-files-race` 也通过，日志使用同一 fd-copyout-green 前缀。增量 std 全部通过，日志 `/tmp/pr2357-fd-copyout-green-std.log`。

以下两类 flag 独立核验，不能把 pidfd 的失败回滚套用到 parent_tid 的发布后写入。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| clone(CLONE_PIDFD 输出错误) / X56、G220 | [v7.1 copy_process](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/fork.c#L2305) | FD 耗尽先报 EMFILE；否则坏输出报 EFAULT，不发布孩子 | `sys_clone → do_clone_in_cgroup → prepare_file_like → vm_write` | 正确 | 旧实现和中间实现的直接红例均已确认，同一最终用例四架构通过 |
| clone3(CLONE_PIDFD 输出错误) / X435、G435 | [v7.1 copy_process](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/fork.c#L2305) | 结构参数验证后按相同顺序处理 pidfd 输出 | `sys_clone3 → do_clone_in_cgroup → prepare_file_like → vm_write` | 无法确认 | 共用修复后的实现；本轮未直接覆盖 clone3 的这些错误组合 |
| clone(CLONE_PARENT_SETTID 输出错误) / X56、G220 | [v7.1 kernel_clone](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/fork.c#L2737) | 孩子发布后尽力写 parent_tid，坏指针不取消创建 | `sys_clone → do_clone_in_cgroup → publish_clone → vm_write` | 正确 | 直接 syscall 红例及同一最终四架构绿例均已确认 |
| clone3(CLONE_PARENT_SETTID 输出错误) / X435、G435 | [v7.1 kernel_clone](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/fork.c#L2737) | 结构参数验证后保留发布后的孩子，不因 parent_tid 写失败回滚 | `sys_clone3 → do_clone_in_cgroup → publish_clone → vm_write` | 无法确认 | 共用修复后的实现；本轮未直接覆盖 clone3 的坏 parent_tid |

删除提前写入预检查后，同时清理了失去调用方的 `prepare_user_write` 和 `size_of` 导入；首次最终构建因这两项未清理而被 `-D warnings` 拒绝，清理后再执行上述全部最终绿例。copyout 修复后的完整功能组合静态检查交由新提交 CI；前述本地 92/92 精确对应 FD 预留提交 `d92a45e8f8`。

### 5.34 SysV 段所有者

`ShmSegment`、`ShmManager` 和唯一的 `SHM_MANAGER` 移入现有 `ipc::shm` 域，`IpcPerm` 与权限辅助函数移入 `ipc::permission`，msg 与 shm 调用方直接使用域类型。`syscall::ipc::shm` 保留命令解码、用户输入输出和事务编排；`task::ops` 直接调用 IPC 域清理，不再反向依赖 syscall 导出。`ShmCreation` 承载段创建参数，移除旧八参数构造器的 lint 豁免；统计和按索引查找通过登记表方法完成，不向 syscall 暴露 BTreeMap。这项接口迁移为 VMA 持有具体 SysV 来源提供边界，没有新增登记表或插件接口。

当前仍保留 PID 附接记录及原来的清理行为，第 5.32 节的 VMA 记账尚未实现。最新已发布 `55851f0562` 上，共享 MM 红例仍报告 `SHM_MM before=0 reaped=1 after=0 readable=0 detached=0 remaining=1 cleanup=1`，日志 `/tmp/pr2357-55851-shared-shm-red.log`。核验还确认 `MmInner::try_pin` 拒绝 Retiring 状态的新 pin，但现有 `MmPin::Deref` 仍暴露 `Mutex<AddrSpace>`；VMA 修改没有重新验证生命周期。因此关闭逻辑附接必须串行化已准入的修改，并防止关闭后再次发布，而不能只读取 `is_address_space_live` 后减计数。

接口迁移的 x86_64 kernel 为 184/184；`syscall-test-shm-family`、`syscall-test-shmctl-info`、`syscall-test-msgctl`、`test-shm-deadlock` 以及增量 std 均通过，日志为 `/tmp/pr2357-shm-domain-*`。单包 clippy 在出现新的远端 futex panic 后被主动中止，未把部分检查视为通过；随后与 futex 唤醒合并修复一起执行两包定向检查，98/98 通过，日志 `/tmp/pr2357-wake-shm-clippy.log`；该检查在下述同 key requeue 修复之前结束。

### 5.35 唤醒队列合并

`55851f0562` 的 CI run `34527456646` 结束为 28 成功、5 取消、1 失败。失败 job `103041932517` 在 riscv64 的 `ltp-syscalls-fcntl36` 期间触发 `one futex wait generation cannot enter two live wake batches`，原始日志 `/tmp/pr2357-55851-riscv64-ci.log`。问题是把每任务的 wake_q 节点当成了每等待代际的节点：等待者看到 WAIT_WOKEN 后，可以在旧批次发送前结束这一代并开始下一代。Linux `futex_wake_mark` 先发布出队状态，`wake_q_add_safe` 对已经排队的任务合并通知；futex 的返回计数仍按选中的 futex_q 增加。

Starry 的 `WakeBatch` 现在分别保存 selected 等待记录数和 `ThreadWakeBatch` 的任务队列，后者只负责去重与调度通知。只有 `mark_woken` 成功才增加 selected；wake 限额、wake-op 两次选择、同/异桶 requeue 的 wake 部分及返回值均使用 selected。删除了把合法合并视为错误的断言。`ThreadWakeBatch::push` 在 CAS 前执行完整屏障，失败合并路径也发布先前状态；drain 在释放节点之后、调用 scheduler wake 之前执行匹配的完整屏障，对照 Linux `__wake_q_add` 与 `wake_up_q → wake_up_process`。节点链接、Arc 和外部 lease 的所有权转移保持单次取得和释放。

确定性 kernel 回归把同一真实线程句柄交给两个尚未发送的批次。旧实现第二次计数为 0，修复后为 1；日志 `/tmp/pr2357-futex-coalescing-{red,green}.log`，绿例整组 185/185。该用例证明合并时的计数契约，没有模拟完整的 `mark_woken → finish → begin` 执行时序。Starry kernel 现有 std/Loom 边界另验证 store/load 屏障关系：旧顺序允许合并者与发送者同时错过状态，补齐后通过，日志 `/tmp/pr2357-futex-wake-ordering-model-{red,green}.log`；这是协议模型，不是硬件运行证明。最初试图在 ax-task 中运行该模型时发现其不在 std 白名单，模型没有执行，相关全白名单结果未作为此项证据；最终模型使用已有 Starry kernel 的 host-only Loom 依赖，没有添加假运行时或修改白名单。

实际 riscv64 LTP 分组为 86/86 通过，其中 fcntl36 完成全部 7 项并返回成功，日志 `/tmp/pr2357-futex-fcntl36-riscv64-ltp.log`。独立核验确认 selected 限额、wake-op 返回值、节点复用和引用转移符合这次修改的契约；同时指出两项边界：同 key requeue 仍提前返回 woken、没有计入保持原位的 requeue；底层返回 WakeResult::Unavailable 时的既有交付路径尚需结合运行时准入证明。两项都没有通过放宽测试或加入重试掩盖，本节不宣称全部 futex 语义已经完成。

### 5.36 同键重排计数

Linux 普通 `FUTEX_REQUEUE` 和 `FUTEX_CMP_REQUEUE` 允许源、目标 key 相同，仍逐项计入选中的等待者；只有 PI requeue 拒绝相同 key。`collect_futex_requeue_same_bucket` 原来的提前返回漏掉这些计数，现保留原位遍历并按 `requeue_count` 限额更新等待记录。此处不改变入队、唤醒或桶锁顺序。

`syscall-test-futex-requeue` 先把三个真实 pthread 等待者从 source 重排到 target，以返回的实际移动数确认入队；随后在没有唤醒或信号的阶段，对 target 执行两种同键重排，最后唤醒并 join 全部线程。宿主 Linux 返回 `3 / 3 / 3`；旧 Starry 返回 `0 / 0 / 3`，最外层 xtask 返回 1，日志 `/tmp/pr2357-same-key-red.log`。宿主结果不是 PREEMPT_RT 构建或性能证据。

修复后同一 x86_64 QEMU 用例返回 `3 / 3 / 3`，最外层 xtask 返回 0，日志 `/tmp/pr2357-same-key-green.log`。完整四架构执行和最终提交 clippy 由新提交 CI 继续核验；此前 98/98 不冒充这项后续修改的结果。

以下结论限定本节两种非 PI 的同键、零 wake 重排。X 为 x86_64，G 为 aarch64、riscv64、loongarch64；未运行的架构明确保留验证缺口。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| futex(FUTEX_REQUEUE_PRIVATE，同键) / X202、G98 | [v7.1 futex_requeue](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/futex/requeue.c#L589) | 同键合法；保持等待状态，返回选中的等待记录数 | `sys_futex → ResolvedFutex::requeue_to → collect_futex_requeue_same_bucket`，MM 私有 futex 域和桶锁 | 无法确认 | x86_64 同一直接 syscall 红绿，其他三架构待 CI；实现未再提前返回 |
| futex(FUTEX_CMP_REQUEUE_PRIVATE，同键) / X202、G98 | [v7.1 futex_requeue](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/futex/requeue.c#L589) | 比较通过后同键仍计入重排数，不额外唤醒 | `sys_futex → futex_read_user_nofault → requeue_to → collect_futex_requeue_same_bucket` | 无法确认 | x86_64 同一直接 syscall 红绿；本例不穷举比较失败和错误优先级，其他架构待 CI |

### 5.37 Socketpair 准备顺序

`sys_socketpair` 现在遵循固定 Linux `__sys_socketpair` 的准备边界：先检查非法 type flag，预留两个 FD，在 raw 文件表锁外分别复制两个编号，再创建 socket 和文件，最后逐项安装。`PreparedFileDescriptor::reserve_in` 是单 FD 与双 FD 事务共用的私有预留入口；`prepare_file_pair` 只在两个预留成功后调用 copyout，再调用创建闭包。任一阶段失败都由原守卫撤销预留，socket/file 析构不在文件表锁内。两个文件 Arc 改用可失败分配并将失败映射为 ENOMEM；底层 transport 的既有分配策略未在本节扩展。

`bugfix-usercopy-socket-results` 新增非法 flag、非法 family 与坏指针的组合、创建失败后的编号可见性、跨页第二个输出失败和 FD 耗尽检查。旧 Starry 有四项确定性失败，日志 `/tmp/pr2357-socketpair-order-red.log`；第二个输出失败保留首项的原有正确行为也继续覆盖。宿主 Linux 同一完整用例通过。修复后的 x86_64、aarch64、riscv64、loongarch64 均通过，日志分别为 `/tmp/pr2357-socketpair-order-green-final.log` 和 `/tmp/pr2357-socketpair-final-<arch>.log`。x86_64 kernel 185/185、单包 clippy 92/92 通过。首次 std 使用当前 HEAD 作基线，没有选中软件包，未作为证据；改用 `cargo xtask test --since 5feff518b3` 后实际执行 13 个白名单软件包并全部通过，日志 `/tmp/pr2357-socketpair-final-std-selected.log`。

表中只评价此次准备、复制和回滚路径。FD 上限按已占数量判断的遗留近似，以及完整 socket family/type/protocol 支持仍未由这些用例证明。X 为 x86_64，G 为 aarch64、riscv64、loongarch64。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| socketpair(准备、copyout、失败回滚) / X53、G199 | [v7.1 __sys_socketpair](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/net/socket.c#L1828) | 非法 flag 最先报 EINVAL；预留失败早于 copyout；复制完成才创建 socket；后续失败保留已写编号且不安装 FD | `sys_socketpair → prepare_file_pair → reserve_in → UserPtr::write → create → install`，当前进程共享文件表，回调和析构在表锁外 | 正确 | 原实现四项直接红例；同一完整用例宿主 Linux 和四架构 QEMU 通过，部分输出与描述符数量均验证 |

前一提交 `325e81d987` 的 CI job `103064860703` 在 Axvisor aarch64 HTTP `POST /api/vms/1/pause` 超时。日志 `/tmp/pr2357-325e-axvisor-aarch64-ci-clean.log` 显示此前 start 和 running/guest-entry 查询成功；本节 socketpair 修改属于 Starry，不把它当作该失败的修复。pause 的锁、设备回调和通知链仍在定位，未增加超时或重试。

### 5.38 Shmat 准入引用

`ShmAttachPreparation` 在 `SHM_MANAGER → ShmSegment` 锁内取得临时 nattch 引用，对照 Linux `do_shmat` 在获取 backing file 后增加 `shm_nattch`、在 `out_nattch` 释放的顺序。即使另一个操作执行 `IPC_RMID`，已准入的 shmat 仍保留登记；取消且没有其他附接时才删除登记。普通观察者的 Arc 只保留对象内存，不保持 IPC 登记。这不是任务对象的自定义引用计数，而是 Linux 可观察的附接计账。

backing 准备由 `mapping` 在进入 MM 锁前完成；`SharedMemoryObject::allocate` 这里只建立稀疏页索引，实际物理页仍按 fault 分配。`sys_shmat` 在 MM 事务发布后调用 `record_attachment`，同时维护现有两份 PID/地址索引，再释放 MM 锁，最后释放临时引用。完整的新锁顺序为 MM → SHM_MANAGER → ShmSegment；准入和 backing 准备释放 IPC 锁后才进入 MM。`PublishedPendingTlb` 已发布映射，保留正式附接，不因返回错误而误删。`mark_for_removal` 集中承接原 IPC_RMID 实现；IPC ID 的唯一分配器移入 IPC 域，msg/shm 直接调用，没有兼容重新导出。

确定性 kernel 用例 `rmid_preserves_an_admitted_attach_until_cancelled` 通过生产准入和 RMID 方法构造受控顺序。仅有 Arc 的旧路径在“RMID must preserve a segment already admitted by shmat”失败；补齐临时引用后同一用例及 x86_64 kernel 186/186 通过，日志 `/tmp/pr2357-shmat-admission-{red,green}.log`。该用例验证领域状态交错，不声称已执行两个用户进程的受控并发 shmat。四架构 `syscall-test-shm-family`、`syscall-test-shmctl-info`、`test-shm-deadlock` 均通过；x86_64 `syscall-test-msgctl` 和实际选中的 starry-kernel std 通过，日志 `/tmp/pr2357-shmat-final-*`。单包 clippy 92/92 通过，日志 `/tmp/pr2357-shmat-final-clippy.log`。

本节没有把现有 PID 附接台账当成 VMA 生命周期；共享 MM 红例、分裂/合并、fork/exec、最后用户引用和旧 pin 准入等第 5.32 节内容仍未完成。时间字段当前仍沿用既有 monotonic 纳秒写入，也尚未修正成 Linux 的 real seconds。独立核验未在限定时间内返回终态，不能记作领域审查通过。

### 5.39 NPU 提交超时单位

`f358124a86` 的主 CI `34536467761` 已结束：28 成功、5 取消、1 失败。失败 job `103071038717` 在 `OrangePi-5-Plus-1` 的 native-hardware-smoke 超时；随后同板 network-smoke 成功。原始与清理日志分别为 `/tmp/pr2357-f358-orangepi-ci.log` 和 `-clean.log`。NPU Submit 的多行日志只输出到中途，但普通日志发布走非阻塞记录队列，不能因此断言调用线程阻塞在打印。先前 Axvisor pause 超时在该提交 CI 成功，没有对应 AxVM 修复，仍保留为未解释的间歇性失败。

检查实际厂商 ABI 时发现 `rockchip-npu::SubmitDeadline` 将 `RknpuSubmit.timeout` 当成微秒。固定 Rockchip Linux 提交 [77168c8d 的 rknpu_job_wait](https://github.com/rockchip-linux/kernel/blob/77168c8d5ab82399f65a80e9f807b50ba37cf483/drivers/rknpu/rknpu_job.c#L193) 使用 `msecs_to_jiffies(args->timeout)`，并在与微秒耗时比较时乘以 1000。现按毫秒转换成单调时钟纳秒，修正原来缩短 1000 倍的截止时间；未增大调用者传入的 timeout，也未增加重试。该厂商源码用于确定 RKNPU ABI，不能冒充本计划 Linux v7.1 PREEMPT_RT 的调度验证。

旧测试只重复微秒换算假设，现替换为实际轮询行为：timeout=6000 的请求，在 7ms 时尚未超时且下一次轮询完成。旧实现返回 Timeout，修复后返回成功；`cargo xtask cross-test --arch x86_64 --package rockchip-npu --lib` 的同一用例和全部 6 项通过，日志 `/tmp/pr2357-rknpu-ms-{red,green}.log`。单包 clippy 1/1 和增量 std 的实际 6 项均通过，日志 `/tmp/pr2357-rknpu-ms-{clippy,std}.log`。同一块 `OrangePi-5-Plus-1` 的原 native-hardware-smoke 已通过，租约 `84edebfd-3ded-42c5-b190-fa62cab92947`，日志 `/tmp/pr2357-rknpu-ms-board.log`，包含 `STARRY_NPU_YOLOV8_OK` 和 `STARRY_NATIVE_HARDWARE_OK`。这证明修复后的实际设备路径通过，但不把一次成功当作所有历史间歇性挂起的根因证明。

以下结论只针对提交超时字段。真实硬件完成、中断等待、复位和 DMA 回收的完整证明不由纯轮询用例替代。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| ioctl(RKNPU Submit) / aarch64 29 | [Rockchip Linux 77168c8d rknpu_job_wait](https://github.com/rockchip-linux/kernel/blob/77168c8d5ab82399f65a80e9f807b50ba37cf483/drivers/rknpu/rknpu_job.c#L193) | timeout 按毫秒解释，不应将 6000ms 请求在 7ms 时判为超时 | `sys_ioctl → card1::rknpu_driver_ioctl → ax-driver::rknpu::submit → submit_ioctrl → poll_until_ready` | 无法确认 | 单位换算的确定性红绿、std 6 项及同一板卡原用例通过；未证明完整 ioctl 或全部历史挂起根因 |

### 5.40 Exec 的 robust 清理

`do_execve` 原来在安装新 MM 后直接清空 `robust_list_head`，共享映射中的持锁 owner 因而没有收到 `FUTEX_OWNER_DIED`。现在 exit 和 exec 共用 `release_robust_futexes`，在旧 MM 仍可访问时执行既有 robust walk，再清空登记。exec 的非 leader 身份转交移到该清理之前，遵循固定 Linux `begin_new_exec → de_thread → exec_mmap → exec_mm_release → futex_exec_release` 的顺序；Linux 在 robust owner 比较前已经交换 TID，所以仍写着旧非 leader TID 的锁字不应被本线程清理。

新增 `syscall-test-exec-robust` 使用共享映射保存锁字，通过两个管道把新映像保持在存活状态后再观察结果，避免退出时清理掩盖 exec 的遗漏。它覆盖普通 exec、`CLONE_VM | CLONE_VFORK` 共享 MM 和非 leader exec。非 leader 使用 `pthread_create`，因为客户机 musl 1.2.5 的公开 `clone()` 包装会在系统调用前拒绝 `CLONE_THREAD`；先前的 EINVAL 是测试准备失败，不是 Starry clone 缺陷。宿主静态 musl 的最终同一测试三项通过；宿主配置不是 Linux RT 验证。

最终版测试在旧实现上普通和共享 MM 两项失败，锁字分别仍为 `0x80000003`、`0x80000004`；非 leader 原有行为继续通过。日志 `/tmp/pr2357-exec-robust-final-red.log`，最外层 xtask 返回 1。恢复修复后 x86_64 三项均通过，日志 `/tmp/pr2357-exec-robust-final-x86_64.log`。riscv64、aarch64、loongarch64 同一测试也均三项通过，日志 `/tmp/pr2357-exec-robust-final-<arch>.log`。本轮格式检查通过；提交后的 clippy 和完整回归由精确提交 CI 验证。

本节仅修复清理入口和 TID/MM 交接顺序。本节当时的 `handle_futex_death` 仍用读后写更新锁字，可能覆盖并发设置的 WAITERS；后续第 5.41 节已按 Linux nofault cmpxchg 重试协议修复。PI 标记和遍历错误处理、`clear_child_tid` 的 mm_users 条件、vfork 完成通知以及第 5.32 节 SHM/VMA 生命周期均未因此完成。X 表示 x86_64，G 表示 aarch64、riscv64、loongarch64。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| execve(普通 robust 清理) / X59、G221 | [v7.1 exec_mmap](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/exec.c#L837) | 在旧 MM 中清理登记的 robust owner，再替换映像 | `sys_execve → do_execve → release_robust_futexes → exit_robust_list`，每线程登记与旧 MM | 部分正确 | x86_64 普通和共享 MM 确定性红绿、四架构通过；原子更新及 PI 遗留问题见本节 |
| execve(非 leader 身份转交) / X59、G221 | [v7.1 de_thread](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/exec.c#L987) | 先交换 TID；robust owner 比较使用交换后的可见 TID | `do_execve → transfer_pid_identity → release_robust_futexes`，进程 leader 身份和当前线程 | 无法确认 | 四架构与宿主非 leader 用例通过；本例未直接读取新映像 gettid/getpid，不能独立证明完整身份协议 |
| execveat(robust 清理) / X322、G281 | [v7.1 exec_mm_release](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/fork.c#L1502) | 使用同一旧 MM 清理协议 | `sys_execveat → do_execve → release_robust_futexes` | 无法确认 | 本次新回归直接使用 execve，尚无 execveat 独立入口证据 |
| exit(robust 清理入口收敛) / X60、G93 | [v7.1 futex_cleanup](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/futex/core.c#L1407) | 清理 robust 登记后清空指针，再继续 MM release | `sys_exit → do_exit → release_robust_futexes` | 无法确认 | 保留既有 walk 顺序；尚待本次退出回归和 CI |
| exit_group(robust 清理入口收敛) / X231、G94 | [v7.1 exit_mm_release](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/fork.c#L1496) | 发布组退出后，各退出线程执行自己的 robust 清理 | `sys_exit_group → do_exit → release_robust_futexes` | 无法确认 | 本次入口收敛不证明全部组退出竞争 |

### 5.41 Robust 锁字原子更新

`handle_futex_death` 原先先读取 owner，再写回 `OWNER_DIED`。用户态在两步之间发布 `WAITERS` 时，旧写入会覆盖等待位，也不会发出所需唤醒。现在 `update_robust_owner_nofault` 使用 `compare_exchange_user_u32_nofault`，比较失配后重新读取并检查 owner，只有成功替换后才按该次锁字决定唤醒。读缺页、写缺页和 LL/SC 重试分别通过 `RobustOwnerError` 返回外层：任务上下文解决相应访问权限的缺页，只有 LL/SC 耗尽才让出 CPU。顺序对应固定 Linux `kernel/futex/core.c` 的 `handle_futex_death`，不会把只读访问错误一律当作写缺页。

`ax-cpu::user::user_cmpxchg_u32` 集中暴露“返回观察值”的契约，不增加可变 task 布局或调度泛型。x86_64 使用 locked cmpxchg；AArch64 使用 LDXR/STLXR 和成功后的 DMB；RISC-V 使用 LR.W/SC.W.AQRL；LoongArch 使用 LL.W/SC.W 和 DBAR。LL/SC 沿用有界重试错误，架构异常表覆盖读与条件存储。RISC-V 用现有 XLENB 宏区分 RV32/RV64，避免在 RV32 发出 sext.w；RISC-V/LoongArch 对高位 u32 做与加载指令一致的符号扩展。普通 Rust 引用不指向用户锁字，缺页解析和 MM 锁不进入原子指令区间。

同步 dev `822a34a6c9` 的 ax-cpu 重构时，各架构汇编迁入 `components/axcpu/src/arch/<arch>/user_atomic.S`，公共入口统一从 `ax_cpu::user` 导出；FP clone 用 `TaskContext::task_anchor()` 核验绑定，不让 CPU 后端重新依赖运行时的 `ExecutionContextHeader`。`ax-runtime` 继续拥有准备和取消令牌，`prepared.commit()` 后立即调用 `switch_to()`。迁移后定向 `cargo xtask clippy --package ax-cpu` 40/40 通过；四架构 Starry kernel 的 x86_64/riscv64/loongarch64 各 187/187、aarch64 188/188 通过，记录在 `/tmp/pr2357-dev-migration-{cpu-clippy,kernel-<arch>}.log`。增量 `cargo xtask test --since fedc660263` 实际选中 19 个软件包并全部通过，日志 `/tmp/pr2357-dev-migration-std.log`。这组结果验证本次迁移，不替代其余生命周期待办或整套 CI。

确定性 kernel 回归在真实 runtime MM 和任务中映射锁字，在内核读取 owner 后、更新前注入一次 WAITERS 发布，不使用 fake TaskRuntime/TaskSystem/CpuLocal。保留旧读后替换顺序时，同一断言收到 `0x40000000` 而非 `0xc0000000`，最外层 xtask 返回 1，日志 `/tmp/pr2357-robust-cas-red.log`。修复后 x86_64 kernel 187/187 通过；最终用例还检查比较失配不修改、高位 expected 成功匹配、只读页写错误和未映射地址错误，日志 `/tmp/pr2357-robust-cas-final-x86_64.log`。riscv64/loongarch64 kernel 187/187、aarch64 188/188，以及四架构 exec robust 回归均通过，日志 `/tmp/pr2357-robust-cas-final-{kernel,exec}-<arch>.log`。LoongArch 的 grouped runner 捕获成功用例输出，其程序通过标记和非零失败传播仍保留。

独立汇编核验发现最初无条件 sext.w 破坏 RV32，已用宿主 clang 对实际汇编片段得到同一命令的编译红绿，日志 `/tmp/pr2357-rv32-cas-{red,green}.log`。项目没有单独汇编片段的 xtask 入口，这项编译只证明 RV32 指令合法，不替代四架构内核运行。限定范围核验未发现其他新增 ABI 或内存序缺陷，不冒充完整调度/unsafe 合入审查。第 5.40 节登记和 TID 顺序保持；PI 标记、遍历错误处理、MM/vfork 与 SHM/VMA 遗留项仍分别收尾。


`test-mt-execve` 原来安装到 `starry-known-fail`，且 NULL argv 子进程重启硬编码 `/usr/bin/test-mt-execve`。恢复发现后先得到路径 ENOENT 红例；改用 `/proc/self/exe`，断言不变，四架构完整用例通过，现重新纳入 grouped CI。日志 `/tmp/pr2357-robust-cas-mt-exec-x86_64.log` 是路径红例，修复后的 x86_64 为 `-mt-exec-path-green.log`，其余为 `-mt-exec-<arch>.log`。其中 Phase 7 明确检查新映像 gettid 等于 getpid、原始 SYS_exit 执行 robust 清理，以及等待者观察到 OWNER_DIED；这补充了第 5.40 节新用例未直接读取新身份的限制。

以下结论只覆盖普通 robust owner 的原子更新和本节经过的入口，不涵盖 PI robust list 或其余生命周期遗留项。X 表示 x86_64，G 表示 aarch64、riscv64、loongarch64。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| execve(普通 robust owner 更新) / X59、G221 | [v7.1 handle_futex_death](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/futex/core.c#L1097) | 比较替换 owner；保留并发 WAITERS；失配重新检查 owner | `sys_execve → do_execve → release_robust_futexes → handle_futex_death → update_robust_owner_nofault` | 正确 | 实际 MM 受控交错红绿、四架构 kernel/exec 回归通过 |
| exit(普通 robust owner 更新) / X60、G93 | [v7.1 handle_futex_death](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/futex/core.c#L1097) | 成功标记 owner 死亡后，按锁字唤醒普通等待者 | `sys_exit → do_exit → release_robust_futexes → handle_futex_death → wake_robust_futex` | 正确 | 四架构 test-mt-execve Phase 7 真实 SYS_exit 和等待者；同一底层原子交错回归 |
| exit_group(普通 robust owner 更新) / X231、G94 | [v7.1 exit_mm_release](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/fork.c#L1496) | 各退出线程执行相同原子 owner 清理 | `sys_exit_group → do_exit → release_robust_futexes → handle_futex_death` | 无法确认 | 共享清理已验证，尚无本节非空 robust list 的独立 exit_group 入口回归 |
| execveat(普通 robust owner 更新) / X322、G281 | [v7.1 exec_mm_release](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/fork.c#L1502) | 在旧 MM 中执行同一原子 owner 清理 | `sys_execveat → do_execve → release_robust_futexes → handle_futex_death` | 无法确认 | 本轮直接 execve 和 SYS_exit 回归不替代 execveat 独立入口证据 |

前一已发布 `4aab4205ca` 的 CI 34544811759 出现两个失败。ArceOS riscv64 job 103096481641 在 LocalLock owner 唤醒 FIFO 子线程后观察到 Running 而非 Blocked，日志 `/tmp/pr2357-4aab-arceos-rv-ci-clean.log`；PI 重排/抢占链仍需受控定位，未修改断言。OrangePi job 103096481776 中 NPU 已退出 0，后续输出含 NUL 并停滞、ARP 继续，日志 `/tmp/pr2357-4aab-orangepi-ci-clean.log`；不能归因为 NPU submit 超时，也不能据后续重跑声称修复。本节原子更新不被当作这两个失败的根因修复。

### 5.42 队列中的 PI owner 抢占

`apply_pi_schedule_update_in_rq` 原先对队列中的 owner 完成 PI 重排后，无条件返回立即抢占。固定 Linux `switched_to_rt/prio_changed_rt` 则只让优先级严格高于当前任务的 RT owner 请求抢占。现在同一个 rq 事务在重排后调用既有 `wakeup_preempt_with_intent`，保留当前抢占状态和 FIFO 同优先级顺序，提交后才通知 owner CPU。`PiRqFollowup` 分别携带抢占决定和维护工作；不需要抢占时，定时器及平衡维护仍能交给 owner，不用抢占位代替工作通知。Fair 使用完整 rq 的既有判断，已存在立即抢占时不重复取消当前请求保护。

`pi_boost_checks_current_priority` 使用真实 ArceOS 调度器：CPU 0 owner 持有 mutex，FIFO 40 waiter 阻塞并捐赠；CPU 0 的 FIFO 80 或 60 observer 在短暂 IRQ-save 区间观察请求，CPU 1 把 waiter 提到 FIFO 80。owner 保持可运行，观察完成后还检查其有效策略确实已变为 FIFO 80。相同优先级必须没有立即抢占，低优先级 observer 必须有立即抢占，随后完整释放锁并 join 所有线程。没有 fake runtime，也未放宽原 LocalLock 断言。

最初错误使用 `current_cpu_needs_resched()`，它还包含 owner 维护工作，因此早期 `/tmp/pr2357-pi-equal-resched-{red,green}.log` 不用于证明抢占位。现在在已有 fault-injection 功能下用只读 `current_immediate_preemption_requested` 直接观察真实位，没有新建辅助状态。最终同一测试在原 PI 实现上得到 true 而非 false，日志 `/tmp/pr2357-pi-queued-final-red.log`；修复后的四架构 ArceOS `--test-group rust --test-case all` 均通过，日志 `/tmp/pr2357-pi-queued-all-<arch>.log`，包含正反向新检查及原始 LocalLock 用例。合并 dev 的 ax-cpu 重构后，四架构 `task-pi-mutex` 再次全部通过（`/tmp/pr2357-pi-post-merge-<arch>.log`），ax-task 定向 clippy 6/6 通过（`/tmp/pr2357-pi-post-merge-clippy.log`）。

这证明并修复了一个 PI 通知语义错误，但尚不能把原始 LocalLock 偶发失败完全归因于它：未修改的 `task-pi-mutex` 和 RISC-V `all` 本地复跑也曾通过，日志 `/tmp/pr2357-local-lock-rv-{repro,all-repro}.log`。当前运行 owner 的 PI 更新分支仍保留旧的立即重调度，需继续按运行中 RT/DL 的 class hook、配额和定时器生命周期核验。独立探查未及时返回终态，不记作审查通过；完整计划和 OrangePi 停滞仍未完成。

Linux 顺序依据为固定 v7.1 的 [rt_mutex_setprio](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/sched/core.c#L7584)、[switched_to_rt/prio_changed_rt](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/sched/rt.c#L2436) 和 [prio_changed_dl](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/sched/deadline.c#L3389)。本节通过的是 ArceOS 真实锁和调度路径，不能替代 Starry 每个 scheduler syscall 的独立兼容性结论。

### 5.43 EL0 PMU 测试创建接口

合入 dev 新 CPU 测试后，`test-suit/arceos/cpu/user-entry/src/aarch64.rs::run_cpu` 仍使用已删除的 raw 创建与独立 join 函数，导致 CI `34552205250` 的 OrangePi PMU job `103118430514` 在 `pmu-user` 编译失败。现改用 `thread::builder`、`UserContextOptions`、`prepare_user_thread().publish()` 和 `ThreadHandle::join`，保留原栈大小、MM 对页表及 backing pages 的所有权和所有硬件断言。没有恢复兼容入口。

同一 `cargo xtask arceos test qemu --arch aarch64 --test-group cpu --test-case user-entry` 先复现两个 E0425 编译错误，迁移后四个 CPU 的 EL0 授权读取、撤销访问和中断现场检查全部通过，日志 `/tmp/pr2357-pmu-api-{red,green}.log`。定向 `cargo xtask clippy --package arceos-cpu-user-entry` 2/2 通过，日志 `/tmp/pr2357-pmu-api-clippy.log`。这是 QEMU 验证，修复后的 OrangePi 实体板卡结果仍由新提交 CI 提供；前一次 fail-fast 取消的其他 ArceOS 项不计为通过。
