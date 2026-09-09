# 线程装配与 Linux RT 生命周期

## 1. 语义基准

本次重构对照 `/home/zhourui/linux-src` 的 Linux v7.1 提交 `8cd9520d35a6c38db6567e97dd93b1f11f185dc6`，分析 `CONFIG_PREEMPT_RT=y` 对应路径。该目录现有 `.config` 未启用 RT，不能作为 RT 运行或性能证据。

### 1.1 所有权

`ax-task` 拥有调度状态和公共线程执行生命周期，`ax-runtime` 提供架构资源装配，Starry 的 `Thread` 持有 Linux 每线程状态并引用共享 `ProcessData`。OS 扩展直接安装到 `ThreadSpec`，不经过内核线程或运行时线程的外层扩展转发。公共创建使用 `ThreadBuilder`，运行队列和唤醒句柄保持非泛型。

### 1.2 顺序对照

Linux 顺序是迁移验收条件，不要求复制 C 对象布局。每条路径必须同时检查发布者、观察者和资源释放责任。

| Linux 基准 | 当前入口 | 目标保证 | 验证 |
| --- | --- | --- | --- |
| `dup_task_struct/copy_process/sched_fork` | `ThreadBuilder`、`create_thread` | 完整初始化后登记，保持 `New`，普通 wake 不激活 | 创建失败回滚、提前唤醒 |
| `wake_up_new_task` | `PreparedThread::stage/activate` | stage 不入队；身份发布后首次激活 | staged 状态及取消 |
| `try_to_wake_up/rt_mutex_schedule` | `ThreadLifecycle`、`pi_park_current_once`、`PiWaitToken` | 普通通知触发条件重检，锁所有权只由 PI handoff/claim 转移 | PI 锁与普通唤醒交错 |
| `context_switch/finish_task_switch` | `execute_switch_plan`、runtime switch tail | 先转移上下文，再 release 发布 prev off-CPU | SMP 切出与迁移 |
| `schedule_tail` | 新线程 trampoline | 首次执行前完成切换尾部和抢占控制权交接 | 首次入口 IRQ/抢占状态 |
| `put_task_stack/put_task_struct` | 任务上下文 reaper | 执行资源与对象引用分阶段回收 | 保留管理句柄的退出线程 |

`finish_task_switch` 先读取退出状态，再清除 `on_cpu`；任务引用和内核栈引用具有不同寿命。Linux RT 通过延迟释放避免在原子上下文取得睡眠锁，本项目复用任务上下文 reaper，并单独证明每个被借用对象的读者保护。

### 1.3 锁等待边界

当前 `RawMutex` 是普通 PI sleeping mutex，`BaseSpinLock` 仍是 raw spinning lock；没有 Linux 将 `spinlock_t` 转换成 sleeping RT lock 的独立类型。因此不能把现有 `WAKE_PENDING/PARK_NOTIFIED` 称为 `TASK_RTLOCK_WAIT/saved_state` 的完整实现。普通 PI mutex 使用独立 `PiWaitToken` 判定 handoff，普通唤醒返回条件循环，interruptible 等待由持久中断谓词决定取消。此重构保留这条路径，不把普通 wake 当作取得锁。

Linux RT sleeping spinlock 在已经发布睡眠状态之后嵌套阻塞所需的 saved-state 协议，当前没有相应实现和直接系统回归；这是计划中尚未完成的验收项，不能以现有 PI 测试通过替代。未来实现需分别保存外层睡眠状态与内层锁等待状态，并验证普通通知仅更新外层状态，不能直接恢复内层锁等待。

## 2. 发布与回收

创建令牌拥有取消责任；登记表拥有已转交资源。逻辑退出通知不代表线程已经物理切出，也不代表最终引用已经消失。

### 2.1 创建事务

`PreparedThread` 保存未运行线程，`StagedThread` 表示完成可失败准入的发布事务。首次激活前不得运行 trampoline 等待用户态发布，取消也不需要调度新线程。队列节点在构造阶段预留；CPU 下线、亲和性和远程投递必须与最终激活串行。

`TaskSystem::stage_new_thread` 在登记表和线程调度锁下选择目标 CPU，把 `PreparedMigrationDelivery` 保存在 `ThreadRecord::activation`。该令牌持有 CPU 接收端和 inbox 投递租约；CPU 下线必须等待已有租约退出。亲和性修改先预留新目标，再替换旧令牌，失败不提交新亲和性。策略修改仍由原调度所有者处理，激活前刷新预留投递的负载信息。

`StagedThread::activate` 在 IRQ/抢占保护下消费预留，只执行一次 `New` 到可运行状态的转换。身份发布后的正常路径没有可恢复错误；运行时未安装等违反契约的情况仍作为不变量失败。低层 `TaskSystem::start_thread` 复用同一准入协议，并拒绝绕过持有公共执行对象的创建令牌。

取消先取走并释放预留，再执行 `mark_exited`，把未调用的入口闭包交给任务上下文退出回调释放。`ThreadExecution::finish` 在闭包析构结束后通知等待者；创建令牌不需要启动线程来完成清理。

### 2.2 回收事务

切换尾部释放 CPU 所有权后，任务上下文回收执行资源。OS 扩展由强句柄和在途回调保护，最终析构不能提前。active-MM 令牌继续由独立的最后 CPU 回收协议处理；同 MM 的 user/kernel 转换不能跳过用户返回屏障。

`TaskSystemState::take_exited_execution` 要求线程已经 `Exited` 且不在 CPU 上，没有迁移、deadline 预留或通知、在途回调、inbox 投递及睡眠定时器。它从登记记录中唯一取出 `ThreadResources`，清除调度状态中的上下文和 MM 句柄，并临时持有强管理租约，防止并发 join 先释放 OS 扩展。任务上下文 reaper 在锁外依次释放架构上下文、TLS 和栈，把 MM token 交给独立回收协议；完成后以 release 发布 `execution_reclaimed`。

这个租约只覆盖一次资源释放事务。普通 `ThreadHandle` 和 `ThreadExtensionBorrow` 保留的是任务对象与 OS 状态，不能通过它们访问已释放的执行上下文或栈。当前调用链没有远端栈读取 API，因此不引入没有消费者的栈引用类型；将来增加栈观察者时必须先加入独立栈保护，不能使用管理句柄代替。

最终对象回收继续经过登记表锁、外部租约、generation 检查、调度活动关闭和回调 claim 协议。CPU 当前对象的裸指针借用由 IRQ/抢占 pin 与 on-CPU 所有权保护，inbox 裸指针由投递租约保护。这里的等价性依赖这些读者逐一退出，并非仅凭 `Arc::strong_count` 推断 Linux RCU 宽限期已经结束。敏感上下文丢弃最后一个管理引用只发布 reaper 工作，不直接调用可能取得睡眠锁的 OS 析构。

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

本地交付包括接口迁移、首次激活、分阶段回收、FP 继承修复、系统回归和微基准。公共执行接口与首次激活/回收协议在一个迁移提交中一起收敛，未完全做到计划要求的接口提交与语义提交分离；后续 FP 修复单独提交。没有推送、创建拉取请求或合并。

完整 RT 验收仍有明确缺口：第 1.3 节的 `TASK_RTLOCK_WAIT/saved_state`，第 3.3 节的既有 IRQ 压力失败与整个 host/Loom 库测试链接问题，以及没有直接系统级证据的 syscall 表项。现有 QEMU 验证也没有逐阶段注入真实栈/TLS/架构上下文分配失败、控制 CPU 下线与 activation 的每种交错或单独证明所有弱内存 MM 屏障；不能把两类已测创建失败扩大为这些场景均已验证。合入前仍需要计划要求的调度及 unsafe 领域审查。

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
