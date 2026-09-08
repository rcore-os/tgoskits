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

CI补充发现：七提交版本的x86_64用户态完整套件通过，但随后裸机`axtest_kernel`编译因alarm宿主单元测试的未使用导入失败。该纯算法测试现在明确`not(axtest)`，不把普通`#[test]`编入自定义裸机测试入口；同一`cargo xtask ktest qemu -p starry-kernel --arch x86_64`入口已通过170项，0失败、0跳过。该修正归入alarm迁移提交。

### 2.5 目录描述符锁

`bug-advisory-lock-dir` 的独特行为全部依赖 `O_RDONLY | O_DIRECTORY` 描述符：flock共享/排他锁及释放、不同OFD间读锁可见、写记录锁返回EBADF。固定LTP的flock/fcntl家族未建立目录描述符场景，`flock01`只在普通O_RDWR文件上检查操作成功，不能承担这一回归。按本轮无等效项清理规则移除原程序及CMake，未新增LTP项，也不再宣称这些目录断言被覆盖。静态确认没有遗留运行入口。

### 2.6 chroot 路径边界

`bug-chroot-parent-escape` 原来从jail目录、jail内的tmpfs挂载点和该挂载根进行多级`..`解析，验证父祖目录secret不可见、符号链接不能逃逸及`rmdir("..")`返回EBUSY。LTP `chroot02` 只验证新根内的文件仍可stat，其余chroot01/03/04主要检查权限和错误码，没有这些路径边界断言。本项按无等效规则清理，未增加LTP用例；这些安全边界失去本项回归保护，不宣称已有普通chroot测试等效。IPv6项仍待语义修复，本项与其无依赖，独立提交。

### 2.7 COW 共享计数

`bug-cow-refcount-overflow` 先预触只读页，再保持300个子进程同时存活并共享该页，验证fork不会因窄引用计数溢出返回EFAULT，最后回收全部子进程。LTP `fork07` 是100子进程共享文件偏移，`hugefork01` 只有一次COW fork，均不覆盖超过255个同时共享者的边界。本项按无等效规则清理，未新增LTP用例；原窄引用计数溢出的确定性保护不再由此程序提供。

### 2.8 删除中的目录游标

`bug-dir-cookie-unlink-rmdir` 使用80字节getdents64缓冲区，读取一批后删除该批条目，继续同一目录游标直至EOF并要求rmdir成功；另覆盖64项跨目录rename后删除，均在tmpfs与rootfs执行。LTP getdents01/readdir01静态枚举以及getdents02/readdir21已删除目录FD错误检查都不覆盖这种读删交错。本项按无等效规则移除，目录cookie稳定性、rename后cookie唯一性和最终目录为空的断言不再由此程序提供。

### 2.9 旧 epoll 入口

`bug-epoll-compat-entrypoints` 包含旧epoll_create成功/非法size、raw旧epoll_wait就绪结果、自身epoll_ctl拒绝。固定LTP epoll_create01/02各有raw和libc两个变体，只有x86_64具有这里使用的旧raw入口，因此单独放入`cases-x86_64.txt`；共同候选epoll_ctl02与epoll_wait01分别覆盖九项错误和三种读写就绪组合。后者使用libc入口，不宣称替代原raw epoll_wait直接调用断言。

CMake读取`CMAKE_C_COMPILER_TARGET`的架构前缀，将可选的`cases-<arch>.txt`与共同清单合并并去重。配置回归先确认旧实现不生成epoll_create01 wrapper，再确认新实现仅在x86_64生成；其他三架构不会生成这些入口。实际x86_64运行发现`epoll_ctl02`将目录FD加入epoll时意外成功（应为EPERM），该用例失败而其他三个候选通过。修复在FileLike增加`supports_epoll`能力查询，Directory明确返回false，EntryKey在ADD/MOD/DEL路径创建前拒绝此类对象；目录自身的同步poll结果不变。修复后四架构LTP运行全部通过，epoll_ctl02完成9项、epoll_wait01完成3项，x86_64额外两个create用例各完成4项；共同集生成及每架构实际用例集合检查通过。`cargo xtask test --since origin/dev`通过，定向clippy共92项全部通过，x86_64裸机axtest共170项全部通过。原程序及CMake已清理；四架构日志保存于实施机器`/tmp/starry-ltp-migration-evidence/08-epoll-<arch>-green.log`。

### 2.10 epoll 嵌套拓扑

`bugfix-bug-epoll-topology` 原来检查四条嵌套边成功、第五条返回`ELOOP`、间接环路拒绝、重复FD边精确删除以及并发反向加边恰有一次成功。固定LTP [epoll_ctl04.c](https://github.com/linux-test-project/ltp/blob/3a64d78f58bdceba93ed321e91215fb969a047ed/testcases/kernel/syscalls/epoll_ctl/epoll_ctl04.c) 先建立五个epoll实例组成的四条嵌套边及末端pipe，再拒绝更深嵌套；[epoll_ctl05.c](https://github.com/linux-test-project/ltp/blob/3a64d78f58bdceba93ed321e91215fb969a047ed/testcases/kernel/syscalls/epoll_ctl/epoll_ctl05.c) 检查闭合环路返回`ELOOP`。两个用例各要求一项TPASS，沿用wrapper默认完成门槛。

这是部分替代：上游深度用例接受`EINVAL`或`ELOOP`，不能保留原来的精确errno断言；上游也不覆盖dup别名边的逐条删除和并发反向加边。原程序及CMake已清理，这三类断言明确不再由本项提供。四架构累计LTP全部通过，新增两个用例实际均返回`ELOOP`；x86_64执行64个LTP程序，其他架构各62个，实际集合与配置逐项一致，生成共同集与`cases.txt`逐字相同。日志为实施机器`/tmp/starry-ltp-migration-evidence/09-topology-<arch>.log`。本项未发现新的实现缺陷，没有增加内核改动。

### 2.11 epoll 用户输出缓冲区

`bug-epoll-wait-user-buffer-race` 原来覆盖阻塞后munmap输出缓冲区返回`EFAULT`、跨页部分复制后返回已完成事件数并重排队失败事件、过大maxevents返回`EINVAL`、没有就绪事件时拒绝内核地址和溢出范围。固定LTP [epoll_wait03.c](https://github.com/linux-test-project/ltp/blob/3a64d78f58bdceba93ed321e91215fb969a047ed/testcases/kernel/syscalls/epoll_wait/epoll_wait03.c) 只部分承接不可写输出缓冲区的`EFAULT`，并增加坏FD、非epoll FD、负数与零maxevents检查，共五项TPASS。

原程序及CMake已清理。未承接的是阻塞期间解除映射、部分复制计数与ONESHOT事件重排队、`INT_MAX`数量上限、无就绪事件时的非法/溢出用户范围；静态只读映射不能证明这些竞态和边界。四架构定向LTP均完成五项，实际执行集合和共同集核对通过，无新增实现缺陷。证据保存于实施机器`/tmp/starry-ltp-migration-evidence/10-11-epoll-wait-<arch>.log`，同一轮同时验证下一项独立提交的候选。

从实际四架构根文件系统提取musl后反汇编`epoll_wait`与`epoll_pwait`：前者转发后者，后者使用x86_64编号281、其他架构编号22；x86_64仅在`ENOSYS`时回退232。Starry已实现281，因此这些LTP结果验证`epoll_pwait`路径，不能补回第八项清理的raw旧入口断言。反汇编证据为`/tmp/starry-ltp-migration-evidence/epoll-libc-<arch>.asm`。

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
| getdents64(删除交错) / x86_64:217；aarch64、riscv64、loongarch64:61 | [Linux v7.1 readdir.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/readdir.c) | 目录条目删除及rename后继续同一遍历游标，不因计数型offset跳过仍存活条目 | `sys_getdents64` → Directory迭代游标 → 文件系统目录cookie → DirBuffer写回 | 无法确认 | 原读删交错回归已清理，未增加静态枚举LTP来冒充等效 |
| epoll_create / x86_64:213 | [Linux v7.1 eventpoll.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/eventpoll.c) | 正size返回有效epoll FD；非正size返回EINVAL | `sys_epoll_create` → size校验 → `sys_epoll_create1(0)` → `Epoll::new`及FD表 | 正确 | x86_64 epoll_create01/02 raw与libc变体各4项TPASS；其他架构不存在此raw入口 |
| epoll_ctl(目录ADD) / x86_64:233；aarch64、riscv64、loongarch64:21 | [Linux v7.1 eventpoll.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/eventpoll.c#L2256) | 目标目录没有poll操作，拒绝注册并返回EPERM | `sys_epoll_ctl` → `Epoll::add` → `EntryKey::new` → Directory `supports_epoll=false`，未发布interest | 正确 | LTP epoll_ctl02 x86_64先失败后通过，四架构各9项TPASS；结论限这些输入 |
| epoll_ctl(目录MOD) / x86_64:233；aarch64、riscv64、loongarch64:21 | [Linux v7.1 eventpoll.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/eventpoll.c#L2256) | 目录目标在interest查找前返回EPERM | `sys_epoll_ctl` → `Epoll::modify` → `EntryKey::new`能力检查 | 无法确认 | 修改路径也使用该能力检查；epoll_ctl02目录用例只覆盖ADD，未声称其他路径已实测 |
| epoll_ctl(目录DEL) / x86_64:233；aarch64、riscv64、loongarch64:21 | [Linux v7.1 eventpoll.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/eventpoll.c#L2256) | 目录目标在interest查找前返回EPERM | `sys_epoll_ctl` → `Epoll::delete` → `EntryKey::new`能力检查 | 无法确认 | 删除路径也使用该能力检查；epoll_ctl02目录用例只覆盖ADD，未声称其他路径已实测 |
| epoll_wait / x86_64:232 | [Linux v7.1 eventpoll.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/eventpoll.c) | 返回就绪事件数量、事件位与user data | `sys_epoll_wait` → `sys_epoll_pwait` → `do_epoll_wait` → Epoll `poll_events_with`与用户写回 | 无法确认 | epoll_wait01使用libc；不能仅凭该结果宣称原直接SYS_epoll_wait断言被保留 |
| epoll_pwait / x86_64:281；aarch64、riscv64、loongarch64:22 | [Linux v7.1 eventpoll.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/eventpoll.c) | libc epoll_wait后端的读/写/组合就绪语义；此处不涉及非空sigmask | `sys_epoll_pwait` → `do_epoll_wait` → `with_blocked_signals`、`poll_io`、Epoll事件消费和写回 | 正确 | epoll_wait01四架构各3项TPASS；实际musl反汇编确认使用epoll_pwait，结论限本行空sigmask就绪场景 |
| epoll_ctl(嵌套深度与环路ADD) / x86_64:233；aarch64、riscv64、loongarch64:21 | [Linux v7.1 eventpoll.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/eventpoll.c#L2069) | 最多四条嵌套边；超深或闭环拒绝且不发布新边 | `sys_epoll_ctl` → `Epoll::add_interest` → 全局拓扑锁 → `prepare_nested_link`双向深度扫描 → 成功后提交interest及双向边 | 正确 | epoll_ctl04/05四架构各1项TPASS，实际返回ELOOP；上游04允许EINVAL，未来精确errno回归保护较原程序弱 |
| epoll_ctl(dup别名边DEL) / x86_64:233；aarch64、riscv64、loongarch64:21 | [Linux v7.1 eventpoll.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/eventpoll.c#L2069) | 删除指定FD边，其他别名边仍阻止反向闭环；全部删除后允许反向边 | `sys_epoll_ctl` → `Epoll::delete` → `remove_interest_locked` → `detach_nested_link`按edge id删除双向边 | 无法确认 | 原dup别名边删除回归已清理，LTP04/05未承接该生命周期 |
| epoll_ctl(并发反向ADD) / x86_64:233；aarch64、riscv64、loongarch64:21 | [Linux v7.1 eventpoll.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/eventpoll.c#L2069) | 并发建立相反边时必须拒绝其中一条，避免共同形成环路 | `sys_epoll_ctl` → `Epoll::add_interest` → 全局拓扑锁覆盖校验与提交 | 无法确认 | 原屏障并发回归已清理，LTP04/05只验证串行图操作 |
| epoll_pwait(静态错误输入) / x86_64:281；aarch64、riscv64、loongarch64:22 | [Linux v7.1 eventpoll.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/eventpoll.c) | 坏FD为EBADF，非epoll FD及非正maxevents为EINVAL，就绪事件写入只读页为EFAULT | `sys_epoll_pwait` → `do_epoll_wait`参数/FD检查 → `poll_events_with` → `write_epoll_event`逐事件用户写回 | 正确 | epoll_wait03四架构各5项TPASS，实际libc后端已反汇编确认 |
| epoll_pwait(解除映射、部分复制与极限范围) / x86_64:281；aarch64、riscv64、loongarch64:22 | [Linux v7.1 eventpoll.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/eventpoll.c) | 等待后复制重新检查映射；部分完成优先返回数量，未交付事件保留；超大数量和溢出用户范围被拒绝 | `do_epoll_wait` → `check_epoll_events_access` → `poll_events_with`逐事件消费与复制失败恢复 | 无法确认 | 原专门回归已清理，静态只读页LTP没有承接上述断言 |
