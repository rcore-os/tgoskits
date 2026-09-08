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

### 2.12 EPOLLET 第二段数据

`bug-epollet-second-chunk` 原来在TCP回环连接上检查空闲时不产生幻事件，并重复32轮“写A、等待、读尽、写B、等待、读尽”。固定LTP [epoll_wait06.c](https://github.com/linux-test-project/ltp/blob/3a64d78f58bdceba93ed321e91215fb969a047ed/testcases/kernel/syscalls/epoll_wait/epoll_wait06.c) 以非阻塞pipe验证写满后的IN事件、半读后不重复产生边缘事件、读空后的OUT事件，并检查FD、事件位与满/空时`EAGAIN`，共九项TPASS。

这里只部分承接EPOLLET下的数据就绪事件；半读后不重复通知是LTP增加的检查。pipe不能证明TCP已连接套接字的OUT就绪不导致IN幻事件，也没有保留读尽后第二次写入重新交付IN及32轮重复检查；这些覆盖损失随原程序和CMake清理明确记录。四架构均完成九项TPASS，执行集合和共同集核对通过，未发现新缺陷。验证复用上一项记录的同一轮四架构日志，测试替换与完成门槛保持独立提交。

### 2.13 ext4 目录操作

`bug-ext4-dir-ops` 的替代项是`rmdir01/02`、`rename03/04/05/07`和`readdir01`，处理为部分替代并修复。rmdir两项在tmpfs验证空目录删除和九种错误；rename与readdir必须实际完成ext4阶段，并继续执行上游发现的其他文件系统。原程序的ext4 rmdir后置状态、失败后兄弟及内容完整性、rename后父目录引用、重建旧名字、pip升级序列、32/80字节读删交错和HTree长目录SEEK_END/回卷断言未被完整承接。`getdents01`包含当前musl及部分架构不可用的变体，不能隐藏其TCONF后宣称替代getdents64。

本项先用`rmdir02`证明挂载点和`.`错误返回ENOTEMPTY，再分别修复为EBUSY和EINVAL；并恢复loop来源的ext4挂载、消除ext4根引用环、修复传输游标以及LTP文件系统漏跑门禁。后续暴露的退出cwd滞留和unshare上下文未登记问题，由`ltp-isolation-exit-fs`确定性复现：旧实现分别在等待子进程退出后错误EBUSY，以及活跃私有cwd仍存在时错误允许卸载。其调度与进程交接控制没有完整等效LTP用例，作为此次新增缺陷的配套回归保留在LTP分组的native阶段，不计入上游LTP数量，也不冒充原程序的完整替代。

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
| epoll_ctl(管道EPOLLET注册) / x86_64:233；aarch64、riscv64、loongarch64:21 | [Linux v7.1 eventpoll.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/eventpoll.c) | 为读/写pipe端登记带用户FD数据的边缘触发兴趣 | `sys_epoll_ctl` → `EpollFlags::EDGE_TRIGGER` → `Epoll::add_interest` → `TriggerMode::Edge`及PollRegistrar | 正确 | epoll_wait06四架构验证已登记读写端实际边缘通知 |
| epoll_pwait(管道EPOLLET消费) / x86_64:281；aarch64、riscv64、loongarch64:22 | [Linux v7.1 eventpoll.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/eventpoll.c) | 交付IN后半读不重复通知；完全读空后交付OUT，返回对应FD与事件位 | `do_epoll_wait` → `poll_events_with` → `EpollInterest::consume`匹配就绪与触发模式 → 用户事件写回 | 正确 | epoll_wait06四架构各9项TPASS；实际musl后端已确认 |
| epoll_pwait(TCP EPOLLET重复数据交付) / x86_64:281；aarch64、riscv64、loongarch64:22 | [Linux v7.1 eventpoll.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/eventpoll.c) | 无匹配就绪时不返回幻事件；TCP读尽后新数据再次触发IN | `do_epoll_wait` → Epoll兴趣队列及Socket Pollable注册/通知 → 事件消费与写回 | 无法确认 | 原TCP空闲与32轮两段数据回归已清理，pipe LTP未承接TCP专有路径 |
| mount(ext4 loop来源) / x86_64:165；aarch64、riscv64、loongarch64:40 | [Linux v7.1 fs/namespace.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namespace.c) | 本轮flags=0的loop镜像挂载成功，后端引用持续到最终文件系统使用者释放 | sys_mount → mount_ext4 → LoopMountLease → new_filesystem_from_file → FileImageDevice → Ext4Filesystem | 正确 | 七项LTP中五项实际完成ext4阶段；ext4漏跑门禁及挂载EIO先红后绿 |
| umount2(普通卸载) / x86_64:166；aarch64、riscv64、loongarch64:39 | [Linux v7.1 fs/namespace.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namespace.c) | 活跃cwd阻止卸载；等待子进程退出后不得因已退役task仍持有cwd而返回EBUSY | sys_umount2 → is_mount_busy → FS_REGISTRY及FD表 → commit_unmount；do_exit提前释放FS_CONTEXT | 正确 | exit_fs_context.c的普通及unshare场景确定性先红后绿；LTP卸载不再依赖EBUSY重试 |
| umount2(缓存写回范围) / x86_64:166；aarch64、riscv64、loongarch64:39 | [Linux v7.1 fs/super.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/super.c) | 卸载目标文件系统只同步其缓存，不因无关文件系统映射忙而失败 | sys_umount2 → sync_filesystem_cached_files → 缓存backing的FilesystemOps身份过滤 → 目标flush | 无法确认 | 确定性缓存端点回归先红后绿，且保留目标自身ResourceBusy；真实system成功，但尚无系统级确定性跨FS忙交错证据 |
| umount2(MNT_DETACH) / x86_64:166；aarch64、riscv64、loongarch64:39 | [Linux v7.1 fs/namespace.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namespace.c) | 延迟卸载摘除挂载树后，已有打开文件继续拥有文件系统及来源 | sys_umount2 → detach_mount → Mountpoint共享Ext4MountLease → retire_filesystem_cache及目录缓存清理 → FileImageDevice释放LoopMountLease | 正确 | 四架构native回归：两个bind别名依次detach后打开文件仍可读写；最后close释放loop并写回脏页，重新挂载读回相同数据；同一释放回归先红后绿 |
| rmdir / x86_64:84 | [Linux v7.1 fs/namei.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namei.c) | 空目录删除；非空ENOTEMPTY、挂载点EBUSY、最终点组件EINVAL | sys_rmdir → sys_unlinkat(AT_REMOVEDIR) → FsContext::remove_dir → Location::unlink | 正确 | rmdir01及rmdir02全部10项断言；02错误码回归先红后绿 |
| unlinkat(AT_REMOVEDIR) / aarch64、riscv64、loongarch64:35 | [Linux v7.1 fs/namei.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namei.c) | rmdir的libc后端，保持目录删除及九种错误条件 | sys_unlinkat → with_fs → FsContext::remove_dir → Location::unlink | 正确 | 三架构rmdir01/02各10项TPASS；已核验镜像musl实际调用35 |
| unlinkat(AT_REMOVEDIR) / x86_64:263 | [Linux v7.1 fs/namei.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namei.c) | 与rmdir共享目录删除语义 | sys_unlinkat → with_fs → FsContext::remove_dir | 无法确认 | x86_64本轮由libc调用raw rmdir，未把内部复用当成此编号的直接运行证据 |
| rename / x86_64:82 | [Linux v7.1 fs/namei.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namei.c) | 文件及空目录替换保持inode身份；非空目录目标及类型不匹配报错 | sys_rename → sys_renameat2(flags=0) → resolve_parent → Location::rename_with_options | 正确 | rename03/04/05/07在ext4及tmpfs各完成11项断言；已核验musl入口 |
| renameat / aarch64:38 | [Linux v7.1 fs/namei.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namei.c) | libc rename后端，两个目录FD均为AT_FDCWD | sys_renameat → sys_renameat2(flags=0) → Location::rename_with_options | 正确 | 同组LTP在ext4及tmpfs通过；镜像musl调用38 |
| renameat2(flags=0) / riscv64、loongarch64:276 | [Linux v7.1 fs/namei.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namei.c) | libc rename后端，不附加特殊renameat2 flag | sys_renameat2 → resolve_parent → Location::rename_with_options(REPLACE) | 正确 | 同组LTP在ext4及tmpfs通过；镜像musl调用276 |
| getdents64(静态枚举) / x86_64:217；aarch64、riscv64、loongarch64:61 | [Linux v7.1 fs/readdir.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/readdir.c) | 枚举目录中的固定条目集合 | sys_getdents64 → Directory目录游标 → ext4/tmpfs目录节点 | 正确 | readdir01在两个文件系统各核对10个前缀条目；不含原读删交错及HTree SEEK_END断言 |
| open(普通loop设备) / x86_64:2 | [Linux v7.1 fs/open.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c) | 打开文件描述持有设备使用引用，最后关闭才释放 | sys_open → sys_openat → add_to_fd → LoopDevice::open；File::drop配对close | 正确 | LTP设备发现、绑定、mkfs、挂载和清理实际执行并重复复用loop0；结论限普通打开配对 |
| openat(普通loop设备) / aarch64、riscv64、loongarch64:56 | [Linux v7.1 fs/open.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c) | 普通块设备打开也需要使用引用，不仅O_EXCL路径 | sys_openat → add_to_fd → LoopDevice::open；File::drop配对close | 正确 | 同一LTP设备生命周期在三架构实际执行 |
| open(O_PATH设备) / x86_64:2 | [Linux v7.1 fs/namei.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namei.c) | 仅取得loop路径句柄，其关闭不能释放普通打开者的设备引用 | sys_open → add_to_fd的O_PATH提前返回 → File::drop跳过设备close | 正确 | x86_64 native回归在AUTOCLEAR绑定上打开/关闭O_PATH，随后GET_STATUS64仍成功；结论不含ptmx创建语义 |
| openat(O_PATH设备) / x86_64:257 | [Linux v7.1 fs/namei.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namei.c) | 仅取得loop路径句柄，其关闭不能释放普通打开者的设备引用 | sys_openat → add_to_fd的O_PATH提前返回 → File::drop跳过设备close | 无法确认 | 本轮x86_64 libc open调用旧open入口，未把内部复用当成257编号的直接证据 |
| openat(O_PATH设备) / aarch64、riscv64、loongarch64:56 | [Linux v7.1 fs/namei.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namei.c) | 仅取得loop路径句柄，其关闭不能释放普通打开者的设备引用 | sys_openat → add_to_fd的O_PATH提前返回 → File::drop跳过设备close | 正确 | 三架构同一native回归通过；已核验musl open使用openat后端；结论不含ptmx创建语义 |
| close(loop设备) / x86_64:3；aarch64、riscv64、loongarch64:57 | [Linux v7.1 fs/open.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c) | 最后一个打开文件描述释放设备引用；AUTOCLEAR在最终使用者离开时解绑 | sys_close → release_locks_on_close → File::drop → LoopDevice::close → 锁外释放LoopBinding | 正确 | 连续五项LTP均重新取得loop0并成功格式化、挂载、卸载和解绑 |
| ioctl(LOOP_SET_FD) / x86_64:16；aarch64、riscv64、loongarch64:29 | [Linux v7.1 drivers/block/loop.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/drivers/block/loop.c) | 空闲loop绑定后端文件，已有绑定不能被替换 | sys_ioctl → Device::ioctl_for_task → LoopDevice::bind → 单锁发布LoopBinding | 正确 | LTP tst_attach_device实际绑定；结论限成功绑定及随后I/O |
| ioctl(LOOP_SET_STATUS) / x86_64:16；aarch64、riscv64、loongarch64:29 | [Linux v7.1 drivers/block/loop.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/drivers/block/loop.c) | 已绑定设备更新旧ABI的文件名与flags | sys_ioctl → LoopDevice::ioctl → state.binding_mut，名称和flags在同一锁内发布 | 无法确认 | LTP tst_attach_device实际设置状态；其他offset和flag语义不在本行验证范围 |
| ioctl(LOOP_CLR_FD) / x86_64:16；aarch64、riscv64、loongarch64:29 | [Linux v7.1 drivers/block/loop.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/drivers/block/loop.c) | 挂载仍使用设备时延迟解绑，不能重绑；最后设备引用释放后变为空闲 | sys_ioctl → LoopDevice::ioctl设置AUTOCLEAR/rundown → 最后close释放binding | 正确 | 四架构native回归在挂载及detached打开文件存活时核对GET_STATUS64成功、SET_FD返回EBUSY，最终关闭后GET_STATUS64返回ENXIO |
| ioctl(LOOP_GET_STATUS) / x86_64:16；aarch64、riscv64、loongarch64:29 | [Linux v7.1 drivers/block/loop.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/drivers/block/loop.c) | 空闲设备ENXIO，已绑定状态从一致快照读取 | sys_ioctl → LoopDevice::get_info → state.binding → write_loop_info | 正确 | LTP每项以旧GET_STATUS识别可复用loop0；结论限空闲ENXIO和普通绑定流程 |
| ioctl(LOOP_CONFIGURE) / x86_64:16；aarch64、riscv64、loongarch64:29 | [Linux v7.1 drivers/block/loop.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/drivers/block/loop.c) | 文件后端、名称和flags应一次发布 | sys_ioctl → LoopDevice::bind发布LoopBinding | 无法确认 | 本轮LTP使用SET_FD及旧SET_STATUS，不把该路径标为已验证 |
| ioctl(LOOP_GET_STATUS64) / x86_64:16；aarch64、riscv64、loongarch64:29 | [Linux v7.1 drivers/block/loop.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/drivers/block/loop.c) | 已绑定设备返回AUTOCLEAR状态；空闲设备返回ENXIO | sys_ioctl → LoopDevice::get_info64 → write_loop_info64 | 正确 | 四架构native回归核对绑定状态、AUTOCLEAR及最后关闭后的ENXIO；未承诺offset、size限制或加密字段 |
| ioctl(LOOP_SET_STATUS64) / x86_64:16；aarch64、riscv64、loongarch64:29 | [Linux v7.1 drivers/block/loop.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/drivers/block/loop.c) | 设置AUTOCLEAR后，最终关闭释放绑定 | sys_ioctl → LoopDevice::ioctl → state.binding_mut | 正确 | 四架构native回归设置该flag，并在关闭/重新挂载过程中核对实际释放；其他可设置及只读flag组合未验证 |
| exit_group(单线程进程) / x86_64:231；aarch64、riscv64、loongarch64:94 | [Linux v7.1 kernel/exit.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/exit.c) | 文件系统owner在zombie可见及父进程通知前释放 | sys_exit_group → do_exit → FS_CONTEXT.take并锁外drop → publish_zombie及父进程唤醒 | 正确 | 更高FIFO优先级父进程wait后立即卸载，普通/私有FS上下文两场景先红后绿 |
| exit(线程退出) / x86_64:60；aarch64、riscv64、loongarch64:93 | [Linux v7.1 kernel/exit.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/exit.c) | 退出线程释放自身FS引用，活跃CLONE_FS共享者继续拥有上下文 | sys_exit → do_exit(group_exit=false) → 当前scope的FS_CONTEXT.take | 无法确认 | 本轮辅助回归使用libc _exit的exit_group路径，未把它当成共享线程exit直接证据 |
| wait4 / x86_64:61；aarch64、riscv64、loongarch64:260 | [Linux v7.1 kernel/exit.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/exit.c) | 正常等待返回不得早于子进程自身FS资源释放 | sys_wait4 → zombie状态消费；do_exit在publish_zombie前释放FS_CONTEXT | 正确 | 已核验四架构waitpid的musl后端；确定性回归在wait后执行一次umount2 |
| unshare(CLONE_FS) / x86_64:272；aarch64、riscv64、loongarch64:97 | [Linux v7.1 kernel/fork.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/fork.c) | 独立FS上下文继续承担活跃cwd阻止卸载的语义 | sys_unshare → PreparedUnshare::prepare → FsContext::into_shared登记 → commit替换scope，旧owner锁外释放 | 正确 | 私有cwd活跃时旧实现错误允许卸载；同一确定性回归先红后绿 |
| unshare(CLONE_NEWNS) / x86_64:272；aarch64、riscv64、loongarch64:97 | [Linux v7.1 kernel/fork.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/fork.c) | 挂载命名空间替换仍登记其FS上下文 | sys_unshare → PreparedUnshare → into_shared → commit | 无法确认 | LTP隔离运行经过该路径；尚未单独覆盖该flag的全部挂载边界及错误顺序 |
| setns(挂载命名空间) / x86_64:308；aarch64、riscv64、loongarch64:268 | [Linux v7.1 kernel/nsproxy.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/nsproxy.c) | 目标已无FS上下文时不能继续借用其挂载状态 | sys_setns → 远程FS_CONTEXT快照 → None返回NoSuchProcess；PreparedUnshare发布替换 | 无法确认 | 所有调用者已迁移Option状态；未新增该竞态的直接系统证据 |
| mount(MS_BIND) / x86_64:165；aarch64、riscv64、loongarch64:40 | [Linux v7.1 fs/namespace.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namespace.c) | bind别名保留同一文件系统；摘除另一个挂载不使别名失效 | sys_mount → Location::bind_mount → Mountpoint::bind → Ext4Filesystem::mount_lease共享租约 | 正确 | 四架构native回归在原挂载detach后从bind别名读回原数据，并继续验证最后文件释放 |
| pread64 / x86_64:17；aarch64、riscv64、loongarch64:67 | [Linux v7.1 fs/read_write.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/read_write.c) | 挂载detach后已有文件仍能按偏移读回数据，返回实际读取长度 | sys_pread64 → File::read_at → FileBackend/CachedFile → 持有Location及挂载租约 | 正确 | 四架构native回归核对6字节返回值及内容；重新挂载核对最终写回内容；镜像musl入口已反汇编核实 |
| pwrite64 / x86_64:18；aarch64、riscv64、loongarch64:68 | [Linux v7.1 fs/read_write.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/read_write.c) | 挂载detach后已有文件仍能按偏移写入；最终卸载不能丢失脏页 | sys_pwrite64 → File::write_at → CachedFile；最后Ext4MountLease写回再释放来源 | 正确 | 四架构native回归分别在解绑和detach后写入6字节，并在最后close前留下未fsync的修改，重新挂载读回 |
| fsync / x86_64:74；aarch64、riscv64、loongarch64:82 | [Linux v7.1 fs/sync.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/sync.c) | detached文件仍可把内容同步到持有的来源设备 | sys_fsync → File::sync → FileBackend::sync → CachedFileShared::sync → ext4及FileImageDevice::flush | 正确 | 四架构native回归在detach后调用fsync成功并核对内容；不涵盖设备故障时的错误传播；musl入口已核验 |
| openat(procfs远程挂载信息) / x86_64:257；aarch64、riscv64、loongarch64:56 | [Linux v7.1 fs/proc_namespace.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/proc_namespace.c) | 退出任务已释放fs上下文时，不能继续借用其旧挂载树 | sys_openat → procfs远程FS_CONTEXT快照 → None返回NotFound | 无法确认 | 已迁移Option所有权调用者；本轮没有为目标退出与procfs读取竞态增加直接系统回归 |

## 4. 文件系统挂载修复设计

本节对应第十二项ext4目录测试迁移中发现的挂载缺口，已完成本项四架构定向验证和静态检查。恢复涉及文件系统与设备生命周期，合入前需要文件系统及块I/O维护者审查；本PR不自动合并。

### 4.1 I/O边界

LTP用`mount("/dev/zero", ..., "ext4", ...)`探测支持范围，而当前`mount_ext4`恒返回`ENODEV`，使目录候选只在tmpfs执行。继续保留停用后端无法承接原ext4测试；仅改变探测errno同样不能建立真实挂载。历史全镜像缓存和同步`IQueue::poll_request`依赖已经删除的块接口，也不能恢复。

选择在ax-fs-ng内部用现有`FsBlockDevice`适配有所有权的`FileBackend`，通过一个文件来源构造入口交给Starry的loop挂载使用。`FsBlockDevice`保持内部接口，物理设备的`BlockDeviceHandle`及IRQ完成路径保持原有所有者。文件适配器按需调用`read_at`、`write_at`、`sync`，不复制整幅镜像，不建立软件IRQ或第二个块运行时；底层物理I/O继续经过文件系统现有块运行时。块范围用受检算术验证，短I/O和同步错误必须传播。

Starry设备边界需要转发长度和同步能力，默认行为与原`Device::len`、`Device::sync`一致，loop实现提供真实容量和后端同步。挂载入口验证块设备类型及绑定状态，在FsContext锁外构造文件系统，最后发布挂载；失败时由源租约自动释放打开计数。现有从native handle构造文件系统的调用者保持既有接口。

### 4.2 生命周期

文件适配器同时拥有后端与源设备打开租约，租约必须持续到文件系统最后引用释放。`Mountpoint::set_lifetime_guard`只保护attached状态，lazy detach会提前释放，故不能承载该租约。loop的绑定、打开计数、flags与延迟清理状态应由同一锁保护；`LOOP_CLR_FD`遇到其他使用者时延迟清理，最终关闭后释放绑定，禁止在挂载仍使用设备时重绑到其他文件。实际文件I/O在释放该状态锁后执行。

ext4当前强持有root DirEntry，root Inode又强持有Ext4Filesystem，形成循环。需要让文件系统保存弱root引用，由实际目录/文件消费者保持文件系统存活；最后引用释放时停止MMP工作、完成首次必要的卸载并释放后端。析构不能加入对不确定MMP CLEAN写入的重试，也不能在MMP工作线程自己持有最后引用时自我join。生命周期回归先证明旧引用环不释放后端，再证明修复后最后root引用释放后端；四架构LTP必须实际报告ext4执行与完整断言，并验证loop设备能够重复使用。

这项改动没有镜像格式迁移。代码可整体回退，但测试中写入的文件系统数据仍由正常flush/unmount保证；验证仅使用QEMU快照。性能目标是避免全镜像常驻缓存，复用现有文件缓存并按需I/O，不引入轮询、额外重试或超时扩张。

### 4.3 实际执行门禁

固定LTP的`all_filesystems`会自动排除缺少mkfs的文件系统，并且仅执行tmpfs后仍可返回0。迁移必须由`filesystem-passes.txt`按`=== Testing on <fs> ===`阶段核验源代码定义的TPASS数量，不能只检查总数。共享prebuild安装e2fsprogs，CMake安装真实mkfs可执行文件，使运行依赖扫描同步其动态库；wrapper补齐sbin路径。新增门禁已在缺少ext4执行的x86_64输出上确定性失败。

### 4.4 传输游标缺陷

接通实际ext4格式化后，`readdir01`稳定在挂载时返回EIO。诊断发现`FileBackend::Direct::read_at`请求1024字节却累计返回314571776字节：`ax-io::IoBufMutExt::read_from`的切片特化没有推进目的切片，重复覆盖同一缓冲区直至镜像EOF。同族`IoBufExt::write_to`也没有推进源切片。新增组件回归先验证旧实现的部分读取、连续读取、部分写入和零进展状态失败；修复需在特化实现中推进实际完成的字节数，不能在ext4适配层绕过。另一个同族问题是`BorrowedCursor::read_from`返回累计written而非本次增量，连续两次2字节读取错误返回2、4；独立回归在旧实现失败、修复后通过。`tests/std.rs`原来把不推进切片的错误行为写成预期，本项同步改成源耗尽/目的填满断言。三个新回归及57包`cargo xtask test`已通过；x86_64同一LTP七项通过，五项同时完成ext4和tmpfs且每项均复用`/dev/loop0`。同一七项在另外三个架构也已通过，实际ext4阶段及完成数量均核对成功。

### 4.5 退出与上下文登记

Linux v7.1 `kernel/exit.c::do_exit`在退出通知前调用`exit_fs`。Starry旧实现把`FS_CONTEXT`留在已退出线程的资源scope中，运行时回收前仍被`FS_REGISTRY`视为cwd所有者；LTP上游卸载重试会掩盖这个窗口。新增回归把父子固定在CPU0，父进程使用更高FIFO优先级，在`waitpid`返回后立即执行一次`umount2`，确定性报告EBUSY。第二个场景先`unshare(CLONE_FS)`，证明未登记的新上下文会导致活跃cwd错误地不阻止卸载。

`FS_CONTEXT`现在显式保存`Option<Arc<Mutex<FsContext>>>`，None表示该任务已释放文件系统资源。`do_exit`在关闭描述符后、发布zombie前取出这一owner，在scope临界区外释放。其他CLONE_FS共享者持有自己的Arc，不会被退出线程重置cwd或根目录。`FsContext::into_shared`统一登记初始、fork私有及unshare替换上下文；procfs和setns的远程读取显式处理已释放状态。unshare安装新scope值后把旧文件表和旧FsContext带出临界区再释放，避免最终文件系统卸载在关抢占上下文进行。

该公共类型变更的全部工作区直接调用者已更新，`current_fs_context()`对仍活跃任务保持原有返回类型；退出后调用该当前任务入口属于内部生命周期错误。新回归两种场景在x86_64已先红后绿，四架构均通过；七项新增LTP也在四架构通过，未再触发上游EBUSY卸载重试。累计71个共同LTP用例、x86_64两个旧入口及native辅助程序已在四架构通过；57软件包标准库测试、ax-io两项、ax-fs-ng六项及starry-kernel九十二项静态检查也已通过。后续4.6节的新挂载缓存修复另需重验受影响范围。


### 4.6 最后挂载引用与页缓存

新增loop生命周期回归在x86_64确定性失败：`LOOP_CLR_FD`和lazy detach后已打开文件仍可使用，但关闭最后文件后`LOOP_GET_STATUS64`仍成功。全局`CachedFileRegistry`强持有缓存，缓存的后端inode又持有ext4；单独消除根目录引用环不能释放这条持有链。

`FilesystemOps::mount_lease`提供独立于缓存inode的挂载所有权。每个`Mountpoint`在构造时取得它，bind和命名空间副本也走同一构造路径；字段排在目录引用之后，使最后挂载的dentry先释放。ext4在自身的睡眠锁内发布一个弱`Ext4MountLease`，所有挂载及由`Location`持有的打开文件共享该租约。最后租约析构在同一锁下排除新一代发布；若新一代已经发布，则把缓存交给新所有者。

`retire_filesystem_cache`从现有inode弱索引收集该文件系统的缓存，在回收表锁外写回。成功后发布`retired`状态再移除全局强引用，使并发prune不能把旧缓存恢复到表中；失败的脏缓存保留给现有全局同步路径并记录错误。仍被外部目录对象保存的干净缓存如果重新打开，会重新登记。未开启全局VFS的配置也先写回再清目录缓存，写回失败则保留目录的脏缓存所有权。卸载计划的内部提交改为借用计划，detach把目标持有到拓扑锁释放之后，避免最后租约在拓扑临界区执行I/O。目录缓存还存在父目录缓存子条目、子条目强引用父目录的环；最后租约在写回后调用`DirNode::clear_cached_entries`递归解除该环，不删除磁盘条目。仍有别名挂载或打开文件时不会触发。该公开入口沿用原内部`forget`的缓存清理实现，全部调用者同步更名。本轮其他文件系统使用默认空租约，设备IRQ和物理块运行时不承担这一所有权。同一x86_64生命周期回归现已通过；补充bind别名与最后关闭前不fsync的写入，重新挂载读回数据也通过。证据为实施机器`/tmp/starry-ltp-migration-evidence/12-loop-lifetime-x86_64-{red,green2}.log`和`12-loop-alias-dirty-x86_64.log`。并发prune回归已先红后绿，57软件包标准库测试通过；四架构累计集合均通过，逐程序集合及五个ext4阶段完成数量已核对。最新日志为`12-retire-prune-std-{red,green}.log`和`12-mount-final-<arch>.log`；受影响VFS三项、ax-fs-ng六项及starry-kernel九十二项clippy全部通过；补齐无全局VFS配置的同一路径后，再次完整57软件包标准库测试通过。最终全套system及最新dev重验另按2.2节执行。

本项累计运行日志为实施机器`/tmp/starry-ltp-migration-evidence/12-mount-final-<arch>.log`；共同集生成与清单逐字比较一致，x86_64执行74个程序，另外三个架构各72个程序，均含一个独立native辅助程序。最终标准库测试日志为`12-precommit-std.log`，静态检查日志为`12-mount-clippy-<package>.log`。


### 4.7 最新基线重验

本项提交后重基到`origin/dev`的`fb0e2a20d7`，包含普通卸载的受影响拓扑校验以及板卡iperf2迁移。十二项提交的`git range-diff`均为内容相同，没有冲突，也没有改写锁文件。重基后四架构累计LTP集合、58软件包标准库测试、x86_64内核174项测试，以及VFS三项、ax-fs-ng六项和starry-kernel九十二项clippy全部通过。日志为实施机器`/tmp/starry-ltp-migration-evidence/12-rebase-*.log`；逐程序集合和ext4完成数量复核通过。此前十一提交版本的CI曾因共享iperf3服务繁忙失败，最新推送仍需取得自己的CI终态，不能用本地成功替代。

### 4.8 卸载写回范围

十三项版本`cc0ff68e07`的CI运行`34255649369`在loongarch64完整system中报告`test-per-ns-mounts`卸载私有tmpfs返回EBUSY，其他Starry矩阵随之取消。同一用例单独运行及一次本地完整system均通过，所以不把偶然通过当作根因解决。

核验发现`sys_umount2`先调用全局`sync_all_cached_files`，它会同步所有文件系统的缓存；其他文件的WritebackProtect映射端点返回Busy时会错误阻止本次卸载。Linux v7.1 `fs/super.c::generic_shutdown_super`调用目标superblock的`fs/sync.c::sync_filesystem`，不是同步所有文件系统。新增确定性回归让一个无关文件系统的映射端点始终返回Busy，再同步目标文件系统，旧全局实现必然返回ResourceBusy；日志为`13-unmount-scope-std-red.log`。

`sync_filesystem_cached_files`复用现有缓存登记表和不可变backing的文件系统身份，只对目标所有者写回，保持目标自身错误传播；全局sync仍调用原全局入口。没有新增登记表、重试、错误吞并或超时。回归同时确认无关端点没有被调用、目标端点的ResourceBusy仍保留，修复后及完整58包std通过，日志为`13-unmount-scope-std-green.log`。CI中具体EBUSY来源缺少现场分支记录，因此这是确定性复现并修复的相关范围缺陷，不能宣称已从CI日志直接证明唯一根因；仍需同一完整套件和新推送CI终态确认。 修复后的loongarch64完整system已完成509个程序，全部通过；另外三个架构累计LTP均通过，四架构实际LTP集合和五项ext4阶段数量符合清单，生成共同集逐字一致。ax-fs-ng六项及starry-kernel九十二项clippy全部通过。日志为`13-unmount-scope-full-loongarch64.log`、`13-unmount-scope-<arch>.log`及`13-unmount-scope-clippy-<package>.log`。

