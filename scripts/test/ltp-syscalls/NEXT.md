# Starry syscall 测试续迁记录

## 1. 范围与基准

本轮从 `origin/dev e8c2e66466682528d64e4b8102d5940333beaabb` 开始，继续上一轮已合入的 PR #2322。LTP 固定为 `20260529`、提交 `3a64d78f58bdceba93ed321e91215fb969a047ed`；Linux 对照仍为 v7.1、提交 `8cd9520d35a6c38db6567e97dd93b1f11f185dc6`。本轮先按名称审计 `bugfix-*`，首个可在现有功能内修复的普通缺陷为 `linkat` 绝对目标路径错误校验 `newdirfd`；完成该修复及此前成功迁移后停止新增候选。

### 1.1 执行与提交

`cases.txt` 是实际累计执行集合，`probe-cases.txt` 保存候选。探测期间临时选择候选集合，探测日志与最终累计验证分别保存；失败候选保留在候选清单，不进入最终执行集合。每个原程序只在对应 LTP 完成四架构验证后单独提交，共用用例只安装一次；提交主题写入 `migration.csv` 以免变基使账本 hash 失效。

### 1.3 本轮首个缺陷

`bug-linkat-flags-symlink` 的原测试首先按固定 LTP `linkat01.c` 进行等效审计。LTP 用例第 11、15 项使用绝对目标路径，同时传入非目录或无效 `newdirfd`；Linux v7.1 的 `filename_linkat()` 通过 `filename_create()` 解析目标路径，绝对路径因此忽略该描述符。Starry 原实现直接以 `new_dirfd` 调用 `with_fs()`，修复前分别返回 `ENOTDIR`、`EBADF`，修复后把绝对目标路径的解析基准改为 `AT_FDCWD`。

同一 LTP 用例提供确定性红绿证据：修复前累计 x86_64 集合 `total=90 passed=88 failed=2`，修复后移除不适用的 `linkat02` 后 `total=89 passed=89 failed=0`，`linkat01` 22 项全部 `TPASS`。原测试中 symlink 目标类型、坏用户指针的非法 flags 优先级和 `EEXIST` 等断言，以及 `linkat02` 依赖 ext2 工具的错误组合没有由 `linkat01` 承接，均记录为覆盖损失；不通过放宽 wrapper 或隐藏 `TCONF` 接入。

### 1.2 覆盖边界

部分替代逐条列出未承接断言，不能将同一 syscall 的其他输入视为完整等效。IPv6、fallocate 和 fchmodat2 已记录的失败项本批不重新修复。`bug-fb0-offset` 直接检查 framebuffer 设备偏移，属于设备验证，保留。尚未完成等效审计或缺少通过候选的锁测试也保留，不因旧账本标注“待清理”就直接删除。

## 2. 逐项映射

每小节记录原程序的输入、生命周期断言和上游承接范围。上游源码链接固定到 LTP 提交；日志保存在实施机器 `/tmp/starry-ltp-next-evidence/`。

### 2.8 linkat 路径与 flags

`bug-linkat-flags-symlink` 部分替换为 [linkat01.c](https://github.com/linux-test-project/ltp/blob/3a64d78f58bdceba93ed321e91215fb969a047ed/testcases/kernel/syscalls/linkat/linkat01.c)。该用例承接相对和绝对源/目标路径、目录描述符边界、跨设备链接、目录链接及非法 flags 的 22 项断言。固定 Linux v7.1 的 [`filename_linkat()`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namei.c#L5808-L5883) 先校验 flags，再解析源路径和目标路径；目标路径为绝对路径时，`newdfd` 不参与路径起点选择。

Starry 调用链为 `sys_linkat` → `resolve_at` → `with_fs`/`FsContext::resolve_nonexistent` → `Location::link`。原实现只在源路径的 `resolve_at` 中处理绝对路径，目标路径无条件进入 `with_fs(new_dirfd, ...)`。修复后目标路径以 `/` 开头时改用 `AT_FDCWD`，所以 LTP `linkat01` 第 11、15 项从 `ENOTDIR`、`EBADF` 恢复为成功；这也是本轮首个普通可修复缺陷，之后停止探索新的候选。

未承接的原断言包括：flags=0 对 symlink 本体的硬链接、`AT_SYMLINK_FOLLOW` 对目标文件的硬链接内容、坏 old/new 用户指针下非法 flags 的优先级以及已存在目标的 `EEXIST`。固定 LTP [linkat02.c](https://github.com/linux-test-project/ltp/blob/3a64d78f58bdceba93ed321e91215fb969a047ed/testcases/kernel/syscalls/linkat/linkat02.c) 还覆盖过长路径、ELOOP、EACCES、EROFS、EMLINK，但当前镜像没有 `mkfs.ext2`，运行结果为 `TCONF`，因此没有接入累计集合；原测试只删除已被 `linkat01` 部分承接的专属程序，覆盖损失保留在账本中。

x86_64 修复前日志为 `/tmp/starry-ltp-next-evidence/` 中本轮基线输出，显示累计 90 项中 88 项通过、`linkat01` 两项失败；修复后四架构日志 `linkat01-{x86_64,aarch64,riscv64,loongarch64}-green.log` 分别显示累计 `89/89`、`87/87`、`87/87`、`87/87`，且 `linkat01` 均为 22/22 `TPASS`。完整 `qemu/system` 日志 `full-system-{x86_64,aarch64,riscv64,loongarch64}.log` 分别显示 `515/515`、`513/513`、`513/513`、`513/513`，外层均为 `PASS`。

### 2.1 POSIX 锁死锁检测

`bug-fcntl-deadlock` 替换为 [fcntl17.c](https://github.com/linux-test-project/ltp/blob/3a64d78f58bdceba93ed321e91215fb969a047ed/testcases/kernel/syscalls/fcntl/fcntl17.c)。承接行为：三进程管道同步建立锁等待环；F_SETLKW返回EDEADLK；GETLK核对持锁者及区间。

未承接：原两进程单字节区间；拒绝死锁后显式解锁使另一等待者成功获得锁的后置断言。本项是部分替代，原程序及专属 CMake 清理。

四架构03-candidates-<arch>.log实际通过；fcntl17=1 TPASS；未修改内核或上游测试；最终累计集合另验。提交主题为 `test(starry): migrate POSIX lock deadlock regression to LTP`。

### 2.2 OFD 锁竞争

`bug-fcntl-ofd-lock` 替换为 [fcntl34.c](https://github.com/linux-test-project/ltp/blob/3a64d78f58bdceba93ed321e91215fb969a047ed/testcases/kernel/syscalls/fcntl/fcntl34.c), [fcntl36.c](https://github.com/linux-test-project/ltp/blob/3a64d78f58bdceba93ed321e91215fb969a047ed/testcases/kernel/syscalls/fcntl/fcntl36.c)。承接行为：不同open形成独立OFD，F_OFD_SETLKW串行化线程写入；七种OFD/POSIX读写组合保证块内容一致。

未承接：非阻塞冲突精确EAGAIN/EACCES、F_OFD_GETLK类型报告、非重叠区间即时成功、close(dup)保留锁、最后close释放锁。本项是部分替代，原程序及专属 CMake 清理。

四架构03-candidates-<arch>.log实际通过；fcntl34=1 TPASS,fcntl36=7 TPASS；未修改内核或上游测试；最终累计集合另验。提交主题为 `test(starry): migrate OFD lock contention regression to LTP`。

### 2.3 关闭描述符释放记录锁

`bug-fcntl-posix-close-release` 替换为 [fcntl15.c](https://github.com/linux-test-project/ltp/blob/3a64d78f58bdceba93ed321e91215fb969a047ed/testcases/kernel/syscalls/fcntl/fcntl15.c)。承接行为：dup和独立open关闭任一已参与加锁的FD释放本进程锁，其他进程锁保留；独立子进程分别观察关闭前后冲突。

未承接：关闭从未参与加锁的同inode FD这一精确输入；原同进程GETLK和再次加锁本身不能区分锁释放与自有锁。本项是部分替代，原程序及专属 CMake 清理。

四架构03-candidates-<arch>.log实际通过；fcntl15=12 TPASS；未修改内核或上游测试；最终累计集合另验。提交主题为 `test(starry): migrate POSIX close-release regression to LTP`。

### 2.4 flock 共享与排他

`bug-flock` 替换为 [flock02.c](https://github.com/linux-test-project/ltp/blob/3a64d78f58bdceba93ed321e91215fb969a047ed/testcases/kernel/syscalls/flock/flock02.c), [flock04.c](https://github.com/linux-test-project/ltp/blob/3a64d78f58bdceba93ed321e91215fb969a047ed/testcases/kernel/syscalls/flock/flock04.c), [flock06.c](https://github.com/linux-test-project/ltp/blob/3a64d78f58bdceba93ed321e91215fb969a047ed/testcases/kernel/syscalls/flock/flock06.c)。承接行为：同进程不同open的排他冲突及解锁后成功；跨进程共享/排他组合；排他冲突EWOULDBLOCK及错误参数。

未承接：同进程两OFD的共享锁组合；共享锁阻止排他时精确EWOULDBLOCK（flock04只检查失败返回）。本项是部分替代，原程序及专属 CMake 清理。

四架构03-candidates-<arch>.log实际通过；flock02=4 TPASS,flock04=6 TPASS,flock06=4 TPASS；未修改内核或上游测试；最终累计集合另验。提交主题为 `test(starry): migrate flock exclusion regression to LTP`。

### 2.5 flock 信号中断

`bug-flock-blocks` 替换为 [flock02.c](https://github.com/linux-test-project/ltp/blob/3a64d78f58bdceba93ed321e91215fb969a047ed/testcases/kernel/syscalls/flock/flock02.c), [flock07.c](https://github.com/linux-test-project/ltp/blob/3a64d78f58bdceba93ed321e91215fb969a047ed/testcases/kernel/syscalls/flock/flock07.c)。承接行为：LOCK_NB排他冲突EWOULDBLOCK；无SA_RESTART处理函数中断阻塞LOCK_EX返回EINTR。

未承接：父进程显式解锁后阻塞者成功获得锁；至少50ms阻塞及非阻塞最多50ms的耗时断言；LTP信号场景仍使用上游一秒睡眠。本项是部分替代，原程序及专属 CMake 清理。

四架构03-candidates-<arch>.log实际通过；flock02=4 TPASS,flock07=2 TPASS；未修改内核或上游测试；最终累计集合另验。提交主题为 `test(starry): migrate interruptible flock regression to LTP`。

### 2.6 私有 futex 等待唤醒

`bug-futex-wait-wake` 替换为 [futex_wait03.c](https://github.com/linux-test-project/ltp/blob/3a64d78f58bdceba93ed321e91215fb969a047ed/testcases/kernel/syscalls/futex/futex_wait03.c)。承接行为：线程私有FUTEX_WAIT返回0，FUTEX_WAKE准确唤醒一个等待者；LTP先观察主线程睡眠状态。

未承接：传入5秒相对超时及唤醒前把futex字从0写为1的输入；两者均未单独验证用户页缺页情形。本项是部分替代，原程序及专属 CMake 清理。

四架构03-candidates-<arch>.log实际通过；futex_wait03=1 TPASS；未修改内核或上游测试；最终累计集合另验。提交主题为 `test(starry): migrate private futex wake regression to LTP`。

### 2.7 getcwd 缓冲区与路径

`bug-getcwd-syscall-return` 替换为 [getcwd01.c](https://github.com/linux-test-project/ltp/blob/3a64d78f58bdceba93ed321e91215fb969a047ed/testcases/kernel/syscalls/getcwd/getcwd01.c), [getcwd02.c](https://github.com/linux-test-project/ltp/blob/3a64d78f58bdceba93ed321e91215fb969a047ed/testcases/kernel/syscalls/getcwd/getcwd02.c)。承接行为：raw getcwd坏地址EFAULT、零/短缓冲ERANGE、NULL短缓冲的ERANGE优先；libc getcwd返回当前路径。

未承接：raw成功返回strlen(path)+1的精确数值；固定/tmp返回路径及长度恰好为strlen(/tmp)的NULL短缓冲输入。本项是部分替代，原程序及专属 CMake 清理。

四架构03-candidates-<arch>.log实际通过；getcwd01=5 TPASS,getcwd02=3 TPASS；未修改内核或上游测试；最终累计集合另验。提交主题为 `test(starry): migrate getcwd buffer regression to LTP`。

## 3. 暂缓记录

失败候选没有从候选清单中移除。它们与已通过候选分别记录，不通过更换上游参数、增加超时或删除完成门槛使其进入执行集合。

### 3.1 锁候选失败

`02-lock-probe-x86_64.log` 记录 `fcntl14` 在上游给定的 38 秒期限内未完成，0 TPASS、1 TBROK，wrapper 返回 2。因此 `bug-fcntl-posix-lock` 和 `bug-fcntl-whence` 保留。日志不能区分执行成本与内核进度缺陷，本批不修复或增加 `LTP_TIMEOUT_MUL`。

`fcntl16` 输出三段 TINFO PASSED，但固定源码没有 TPASS 成功报告，wrapper 以 `0 TPASS, expected at least 1` 返回 1。它实际包含部分解锁场景，纠正旧账本关于无部分释放覆盖的说法；但不能通过把 TINFO 当作 TPASS 接入。`bug-fcntl-partial-wake` 与 `bug-fcntl-setlkw-blocks` 保留，也不只选同一原程序的绿色 OFD 候选掩盖 POSIX 候选失败。

### 3.2 其他保留项

`bug-fcntl-fd-mode-ebadf`、`bug-fcntl-len-negative`、`bug-fcntl-ofd-pid-einval`、`bug-fcntl-posix-exit-release`、`bug-flock-failed-upgrade` 没有在本批建立完整等效映射，保留原程序和具体输入。普通 fcntl 无效 FD、正长度区间或 flock 排他冲突，不能分别证明读写模式不匹配、负长度归一化、非零 OFD PID、退出自动释放或失败升级丢弃原共享锁。

### 3.3 独立问题跟踪

每个暂缓问题单独跟踪，共用失败候选关联同一议题，避免为两个原程序重复建单。IPv6 的选项状态与原生地址身份属于不同问题，分别登记；历史问题的议题明确注明尚未在最新 dev 复验。

| 问题 | 议题 | 保留的原程序 |
| --- | --- | --- |
| fcntl14 超时未完成 | [#2341](https://github.com/rcore-os/tgoskits/issues/2341) | bug-fcntl-posix-lock、bug-fcntl-whence |
| fcntl16 无 TPASS 完成报告 | [#2342](https://github.com/rcore-os/tgoskits/issues/2342) | bug-fcntl-partial-wake、bug-fcntl-setlkw-blocks |
| ext4 SEEK_HOLE 返回 EINVAL | [#2343](https://github.com/rcore-os/tgoskits/issues/2343) | bug-fallocate-zero-punch |
| fchmodat2 O_PATH 空路径返回 EBADF | [#2344](https://github.com/rcore-os/tgoskits/issues/2344) | bug-fchmodat2-flags |
| IPV6_V6ONLY 状态与绑定约束 | [#2345](https://github.com/rcore-os/tgoskits/issues/2345) | bug-af-inet6-v4mapped |
| 原生 ::1 端点身份丢失 | [#2346](https://github.com/rcore-os/tgoskits/issues/2346) | bug-af-inet6-v4mapped |

这些议题尚未修复，本批迁移不关闭它们。后续新增暂缓项也应记录复现输入、失败证据与对应议题，不以原程序保留代替问题跟踪。

## 4. 验证与兼容性

本批只替换测试，不改变 Rust 实现。兼容性结论限定到新接入用例实际证明的输入；原程序未被承接的断言不因其他用例同名 syscall 就视为继续覆盖。

### 4.1 验证入口

通过 `cargo xtask starry test qemu --arch <arch> -c qemu/system/ltp-syscalls` 在 x86_64、aarch64、riscv64、loongarch64 串行运行候选。`03-candidates-<arch>.log` 保存固定源码数量契约下的执行结果。逐项提交之后还要运行累计集合和完整 `qemu/system`，核对实际执行程序、重复、已清理程序残留与失败候选误接入。

### 4.2 syscall 对照

固定 Linux 基准与 Starry 状态所有者分别列出。测试替换不改变 syscall 实现，不能把本表中的局部结果外推为完整锁生命周期、超时、用户页缺页或所有错误组合兼容性。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| fcntl(F_SETLKW/F_GETLK) / x86_64:72；其他三架构:25 | [Linux v7.1 posix_locks_deadlock](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/locks.c#L1101) | POSIX 等待环返回 EDEADLK，查询持锁者和区间 | sys_fcntl → dispatch_fcntl → fcntl_setlk/getlk → PosixLockWaitGuard 与 FCNTL_LOCKS，进程身份所有权 | 正确 | fcntl17；四架构候选验证通过，不包含 fcntl14/16 暂缓范围 |
| fcntl(F_OFD_SETLKW/F_OFD_SETLK) / x86_64:72；其他三架构:25 | [Linux v7.1 fcntl_setlk](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/locks.c#L2506) | 独立 OFD 的读写互斥及与 POSIX 锁的竞争，解锁后可继续访问 | sys_fcntl → dispatch_fcntl → fcntl_setlk → FCNTL_LOCKS 的 OFD/POSIX 所有者及 inode 等待队列 | 正确 | fcntl34/36；四架构候选验证通过，不包含 OFD GETLK 或最后关闭语义 |
| close / x86_64:3；其他三架构:57 | [Linux v7.1 filp_flush](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c#L1456) | 关闭同 inode FD 释放当前所有者记录锁，保留其他进程的锁 | sys_close → close_file_like → release_locks_on_close → release_inode_posix_locks | 正确 | fcntl15 的 dup/open/fork 三种组合，四架构验证通过 |
| flock / x86_64:73；其他三架构:32 | [Linux v7.1 flock](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/locks.c#L2214) | OFD 间共享/排他冲突、非阻塞 EWOULDBLOCK、阻塞信号中断 EINTR | sys_flock → flock_op → try_flock_once → FLOCK_LOCKS 及 inode 等待队列 | 正确 | flock02/04/06/07，四架构验证通过 |
| futex / x86_64:202；其他三架构:98 | [Linux v7.1 do_futex](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/futex/syscalls.c#L112) | 私有 WAIT 返回0，WAKE 的计数为1 | sys_futex → FutexContext::resolve → wait_nofault_until/wake，进程私有键及桶队列 | 正确 | 复用 futex_wait03，四架构验证通过 |
| getcwd / x86_64:79；其他三架构:17 | [Linux v7.1 getcwd](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/d_path.c#L413) | 缓冲区过短先 ERANGE，足够长但地址无效 EFAULT；返回当前路径 | sys_getcwd → current_fs_context 当前目录 → absolute_path → vm_write_slice | 正确 | getcwd01/02，四架构验证通过；不证明 raw 成功长度 |
