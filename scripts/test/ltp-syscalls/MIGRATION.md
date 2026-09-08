# Starry syscall 测试迁移记录

## 1. 迁移边界

本轮以 `5a5e6693542ff780a25efa5b36006723ef4306d9` 为起点，逐个审计 Starry 用户态可见语义测试。`migration.csv` 保存原程序、处理状态、LTP 映射、覆盖损失和验证记录；状态为“待审计”的行不代表允许删除。目录扫描只建立候选清单，最终范围由测试断言决定，硬件验证和性能基准保留。

### 1.1 上游基准

LTP 固定为 `20260529`，提交为 `3a64d78f58bdceba93ed321e91215fb969a047ed`。实际运行集合由 `test-suit/starryos/qemu/system/ltp-syscalls/cases.txt` 控制，候选集合不能替代执行证据。Linux 语义以 `v7.1`、提交 `8cd9520d35a6c38db6567e97dd93b1f11f185dc6` 为基准。

### 1.2 提交与证据

每个原程序单独提交，提交主题记录在 `migration.csv` 中，可用 `git log --fixed-strings --grep='<主题>'` 定位提交。完整替代、部分替代后清理、无等效项清理必须分别记录；删除未承接的断言是本轮明确接受的覆盖收缩，不能称为等效覆盖。发现内核缺陷时保留同一回归在错误实现失败、修复后通过的证据。

## 2. 逐项语义

每项记录以固定源码中的断言为依据。LTP 包装器保持退出状态、失败标记和最少通过项检查；`minimum-passes.txt` 的阈值来自源码，不能从绿色日志反推。

### 2.1 CPU 亲和性读取

`affinity-bug-proc-status-affinity` 的 `test_proc_status_affinity` 原来检查四核默认掩码、绑定 CPU1 后的读取结果，以及 procfs 和 sysfs 的表示。替代用例 [sched_getaffinity01.c](https://github.com/linux-test-project/ltp/blob/3a64d78f58bdceba93ed321e91215fb969a047ed/testcases/kernel/syscalls/sched_getaffinity/sched_getaffinity01.c) 检查非空且不超出系统 CPU 数的掩码，以及坏地址 `EFAULT`、零长度 `EINVAL`、不存在进程 `ESRCH`，源码要求四项 `TPASS`。

这个用例不覆盖 `/proc/self/status` 中 `Cpus_allowed`、`Cpus_allowed_list` 的精确字符串、`/proc/cpuinfo` 的 processor 数量，以及 sysfs online、possible、present 的范围。也不能独自证明设置 CPU1 后的读取结果；原程序已清理，这些断言不再由该项迁移保留。

### 2.2 验证状态

本文件与 `migration.csv` 随每项迁移更新。未完成四架构运行的项目不能标为验证通过，未完成的迁移也不计入交付数量。定向入口为 `cargo xtask starry test qemu --arch <arch> -c qemu/ltp-syscalls`；最终还需四架构完整 `qemu/system` 及受影响用例验证。

首项迁移发现 loongarch64 的 `--capture-failures` 吞掉成功 LTP 用例的原始输出。四架构 QEMU 虽然都返回成功，`generate-common.sh` 却确定性返回 1，报告 `the four-architecture LTP intersection is empty`。`starry_system_test_runner.c` 现在在捕获模式下也回放 LTP 成功输出，使完成数量可以被离线复核；原生 C 用例的输出策略不变。修复后 loongarch64 QEMU 通过，同一生成命令返回 0，`cmp` 确认结果与实际 manifest 完全一致，未丢掉任何预期用例。完整日志保存在实施机器 `/tmp/starry-ltp-migration-evidence/01-affinity-*.log`；loongarch64 使用 `01-affinity-loongarch64-green.log`。

### 2.3 CPU 亲和性迁移

`affinity-bug-sched-affinity-migrate` 的 `bug-sched-affinity-migrate` 原来让父进程把正在 CPU0 运行的子进程迁到 CPU1，通过管道握手和 `/proc/self/stat` 的 processor 字段观察完成。固定上游 [getcpu01.c](https://github.com/linux-test-project/ltp/blob/3a64d78f58bdceba93ed321e91215fb969a047ed/testcases/kernel/syscalls/getcpu/getcpu01.c) 将当前任务绑定到掩码中的最高 CPU，再检查 `getcpu` 返回的 CPU 与 NUMA node。这是当前任务迁移的部分替代，不能证明父进程修改子进程亲和性的完成语义，也不保留 procfs 字段和握手断言。

原程序现已清理，四架构 `getcpu01` 各输出一项 `TPASS`（CPU3/node0），最外层 QEMU 返回成功，共同集生成结果恰好包含该用例。日志为实施机器 `/tmp/starry-ltp-migration-evidence/02-getcpu-<arch>.log`。

本项定向验证期间只把 `getcpu01` 放入临时执行 manifest，通过后恢复累计清单并加入该用例。四架构日志必须各自包含完整 LTP 阶段、`TPASS`、逐程序成功和最外层成功；最终完整集合仍需单独验证。该步骤不根据失败结果缩减迁移范围。

### 2.4 闹钟取整

`bugfix-alarm-syscall` 的原测试把剩余约 1.25 秒时返回 2 当成正确行为，内核 `sys_alarm` 也对所有非零小数秒向上取整。[Linux v7.1 alarm_setitimer](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/time/itimer.c#L296) 的规则是：不足一秒但尚未到期时返回 1；其余时间在小数部分达到半秒时进位。因此 1.25 秒应返回 1。

原 C 测试在宿主 Linux `6.8.0-138-generic` 上失败，纠正这项断言后通过；同一纠正测试在未修复 Starry x86_64 上返回 1，日志包含 `round one-second fractional remainder below half a second down`。另外把实际取整算法提取为私有 `alarm_remaining_seconds`，用不读取时钟的边界表验证零、最小非零值、半秒两侧与整秒，避免用调度及时性代替取整算法的确定性证据。旧算法在 `1s + 1ns` 输入下确定性失败（实际2、期望1），完整 `cargo xtask test` 仅 starry-kernel 失败。修复为半秒进位后，同一边界测试与完整57软件包标准库测试均通过；纠正后的同一 x86_64 系统回归也通过。`cargo xtask clippy --package starry-kernel` 的101项检查全部通过。证据文件为实施机器 `/tmp/starry-ltp-migration-evidence/03-alarm-{std-red-full,std-green-full,starry-red,starry-green}.log`。

上游替代用例 `alarm02`、`alarm05`、`alarm06` 分别验证设置与取消返回值、替换和信号交付、取消后不交付信号，源码完成数量分别为 6、3、2。它们不验证兄弟线程共享计时器、创建线程退出后的交付或 `setitimer` 的小数秒互操作；原C测试已清理，这些系统级断言不再由本项保留。四架构三个LTP用例分别完成6、3、2项TPASS，定向QEMU及共同集生成通过；纯取整算法回归保留在内核单元测试。

### 2.5 目录描述符锁

`bug-advisory-lock-dir` 的独特行为全部依赖 `O_RDONLY | O_DIRECTORY` 描述符：flock共享/排他锁及释放、不同OFD间读锁可见、写记录锁返回EBADF。固定LTP的flock/fcntl家族未建立目录描述符场景，`flock01`只在普通O_RDWR文件上检查操作成功，不能承担这一回归。按本轮无等效项清理规则移除原程序及CMake，未新增LTP项，也不再宣称这些目录断言被覆盖。静态确认没有遗留运行入口。

### 2.6 chroot 路径边界

`bug-chroot-parent-escape` 原来从jail目录、jail内的tmpfs挂载点和该挂载根进行多级`..`解析，验证父祖目录secret不可见、符号链接不能逃逸及`rmdir("..")`返回EBUSY。LTP `chroot02` 只验证新根内的文件仍可stat，其余chroot01/03/04主要检查权限和错误码，没有这些路径边界断言。本项按无等效规则清理，未增加LTP用例；这些安全边界失去本项回归保护，不宣称已有普通chroot测试等效。IPv6项仍待语义修复，本项与其无依赖，独立提交。

### 2.7 COW 共享计数

`bug-cow-refcount-overflow` 先预触只读页，再保持300个子进程同时存活并共享该页，验证fork不会因窄引用计数溢出返回EFAULT，最后回收全部子进程。LTP `fork07` 是100子进程共享文件偏移，`hugefork01` 只有一次COW fork，均不覆盖超过255个同时共享者的边界。本项按无等效规则清理，未新增LTP用例；原窄引用计数溢出的确定性保护不再由此程序提供。

## 3. 系统调用兼容性对照

结论仅针对本轮明确检查的路径，不表示整个系统调用在所有输入下都兼容。移除 procfs 或 sysfs 的特定断言后，LTP 的绿色结果不能证明那些文件的表示仍然正确。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| sched_getaffinity / x86_64:204；aarch64、riscv64、loongarch64:123 | [Linux v7.1 syscalls.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/sched/syscalls.c#L1278) | 本轮覆盖非空且不超过配置 CPU 数的掩码；坏地址 EFAULT、零长度 EINVAL、不存在进程 ESRCH | `sys_sched_getaffinity` → `scheduler_thread_id` / `scheduler_task` → 当前 PID namespace 的 `PidView` → ax-task `thread_affinity` 读取线程的 affinity → `vm_write_slice` | 正确 | `sched_getaffinity01`：四架构各四项 TPASS，LTP 阶段均成功，共同集生成与逐字比较成功；结论限本行四项行为 |
| sched_setaffinity / x86_64:203；aarch64、riscv64、loongarch64:122 | [Linux v7.1 syscalls.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/sched/syscalls.c#L1197) | 设置当前或指定线程的 CPU 亲和性；权限检查、用户掩码与允许 CPU 交集影响结果 | `sys_sched_setaffinity` → `check_sched_permission` → `vm_load` → ax-task `set_current_thread_affinity` 或 `set_thread_affinity_and_wait` | 无法确认 | `getcpu01` 四架构证明当前任务绑定CPU3后在CPU3执行；远程任务路径及错误门禁未由该用例验证，不宣称完整兼容 |
| getcpu / x86_64:309；aarch64、riscv64、loongarch64:168 | [Linux v7.1 sys.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/sys.c#L2918) | 返回当前运行CPU及其NUMA节点；本轮验证绑定后CPU与单节点结果 | `sys_getcpu` → HAL `this_cpu_id` → `VmMutPtr::vm_write` 写入CPU与node 0 | 正确 | `getcpu01` 四架构各1项TPASS；结论限非空有效输出指针及当前单NUMA节点场景 |
| alarm / x86_64:37 | [Linux v7.1 itimer.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/time/itimer.c#L296) | 替换进程ITIMER_REAL并返回旧剩余秒数：非零不足一秒保底1，其余以半秒为进位阈值 | `sys_alarm` → `ProcessData::set_interval_timer` → 锁内 `ProcessTimerManager::set_itimer` → `SetITimerOutcome::apply` 发布进程AlarmTarget → `alarm_remaining_seconds` | 正确 | 新边界单元测试先红后绿；同一纠正C回归宿主通过、Starry先红后绿；LTP alarm02/05/06四架构通过；其余架构由libc通过setitimer实现alarm，不宣称存在raw alarm入口 |
| getitimer / x86_64:36；aarch64、riscv64、loongarch64:102 | [Linux v7.1 itimer.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/time/itimer.c) | 读取进程间隔计时器的间隔与剩余时间 | `sys_getitimer` → `ProcessData::get_interval_timer` → 进程accounting.interval_timers锁 → `get_itimer` → `write_itimerval` | 无法确认 | 纠正后的原x86_64回归验证ITIMER_REAL与alarm状态；LTP alarm家族未直接承接全部读取断言 |
| setitimer / x86_64:38；aarch64、riscv64、loongarch64:103 | [Linux v7.1 itimer.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/time/itimer.c#L351) | 替换进程间隔计时器并可选返回旧状态；ITIMER_REAL与alarm共享 | `sys_setitimer` → 读取与转换itimerval → `ProcessData::set_interval_timer` → `SetITimerOutcome::apply` → 可选`write_itimerval` | 无法确认 | 纠正C回归在x86_64验证小数秒设置与alarm交互；其他参数及错误顺序不属于该项已验证结论 |
| flock / x86_64:73；aarch64、riscv64、loongarch64:32 | [Linux v7.1 locks.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/locks.c#L2214) | 目录FD允许共享/排他锁；排他flock不要求O_WRONLY | `sys_flock` → `flock_op` → `lockable` / Directory `inode_key` → FLOCK_LOCKS及按inode等待队列 | 无法确认 | 原目录FD回归已清理，未新增等效LTP运行证据 |
| fcntl(F_OFD_SETLK目录路径) / x86_64:72；aarch64、riscv64、loongarch64:25 | [Linux v7.1 locks.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/locks.c#L2371) | 只读目录允许OFD读记录锁 | `sys_fcntl` → `dispatch_fcntl` → `fcntl_setlk` → `lockable`、`fd_supports_kind`、OFD所有者与FCNTL_LOCKS | 无法确认 | 原目录FD回归已清理，未新增等效LTP运行证据 |
| fcntl(F_OFD_GETLK目录路径) / x86_64:72；aarch64、riscv64、loongarch64:25 | [Linux v7.1 locks.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/locks.c#L2371) | 另一独立OFD可观察目录上的读锁冲突 | `sys_fcntl` → `dispatch_fcntl` → `fcntl_getlk` → `lockable`、OFD所有者、`find_conflict`与写回flock字段 | 无法确认 | 原目录FD回归已清理，未新增等效LTP运行证据 |
| fcntl(F_SETLK目录路径) / x86_64:72；aarch64、riscv64、loongarch64:25 | [Linux v7.1 locks.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/locks.c#L2371) | 只读目录写记录锁返回EBADF | `sys_fcntl` → `dispatch_fcntl` → `fcntl_setlk` → `lockable` → `fd_supports_kind`检查写访问模式 | 无法确认 | 原目录FD回归已清理，未新增等效LTP运行证据 |
| chroot / x86_64:161；aarch64、riscv64、loongarch64:51 | [Linux v7.1 open.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c#L588) | 新root约束后续路径解析，包含挂载与父目录边界 | `sys_chroot` → 当前FsContext锁 → `resolve`、`FsContext::new` → proc_data root/cwd更新；后续路径由FsContext解析 | 无法确认 | 专门路径逃逸回归已清理；LTP chroot01-04未承接上述边界，无新增运行证据 |
| fork / x86_64:57 | [Linux v7.1 fork.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/fork.c#L2802) | 复制进程并维护共享页生命周期；本项原风险为大量同时存活的COW共享者 | `sys_fork` → `sys_clone` → `CloneArgs::do_clone` → 地址空间复制与进程发布 | 无法确认 | 300子进程COW引用计数回归已清理；普通LTP fork不证明该边界 |
| clone(fork的libc后端) / aarch64、riscv64、loongarch64:220 | [Linux v7.1 fork.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/fork.c) | 不带CLONE_VM的进程复制维护共享页生命周期 | `sys_clone` → `CloneArgs::do_clone` → 地址空间复制与进程发布 | 无法确认 | 清理原libc fork程序；该三架构无raw fork入口，未新增300共享者回归 |
