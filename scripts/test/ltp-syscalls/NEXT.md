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

### 2.9 fcntl14 恢复与三个旧 C 锁测试的清理

`bug-fcntl-len-negative`、`bug-fcntl-posix-lock` 和 `bug-fcntl-whence` 曾被尝试部分替换为 [fcntl14.c](https://github.com/linux-test-project/ltp/blob/3a64d78f58bdceba93ed321e91215fb969a047ed/testcases/kernel/syscalls/fcntl/fcntl14.c)。上游默认执行两个变体，每个变体完成 5000 次父子进程锁操作；父进程设置随机 `F_RDLCK`/`F_WRLCK`，子进程通过 `F_GETLK` 检查持锁 pid 和类型，再验证冲突时 `F_SETLK` 返回 `EWOULDBLOCK`、无冲突时可以加锁。正式接入要求两个变体都产生 `TPASS`。

最初按评审提议撤回：x86_64 CI 的第一个变体曾在 38.957 秒处超时。当前分支已在 `cases-x86_64.txt` 恢复上游原用例，并以 `minimum-passes.txt` 中的 `fcntl14 2` 要求两个变体全部完成；不降低默认 5000 次操作数、不增加 `LTP_TIMEOUT_MUL`，也不改变 38 秒上游超时。较早的本地 x86_64 同一原始配置曾连续六轮得到 2 TPASS，但提交 `c0b8173e28` 的 x86_64 CI 第一变体在 39.057 秒失败；叠加退休 MM、memfd 与 COW 优化的提交 `1d45b580ee` 在 x86_64 CI 的第一变体仍于 39.040 秒超时。因此不能据此前的本地通过宣称稳定，仍以本次最终提交的 CI 为完成门槛。其他架构未把该用例列入正式集合。[#2341](https://github.com/rcore-os/tgoskits/issues/2341) 在验证终态前保持开放。

叠加退休 MM、memfd 与 COW 优化后，本地旧分组命令中的 `fcntl14` 曾得到 `2 TPASS`、耗时 69.810 秒，但目标结束后主动停止了剩余 LTP，不能宣称整组通过。变基到 `origin/dev 9722a5e0fb` 后，使用新接入的 `-c qemu/system/ltp-syscalls/fcntl14` 单例运行，追加 VMA 非替换插入免全树查重和 fork 复用匹配的父 `MappingGroup` 之前，第一变体通过、第二变体在 75.527 秒处超时，见 `/tmp/pr2472-latest-dev-fcntl14-20260923.log`；追加后连续三轮单例各得 `2 TPASS` 且外层 `xtask` 退出成功，用时 56.127、56.608、54.879 秒，日志为 `/tmp/pr2472-latest-dev-fcntl14-vma-opt-20260923.log`、`/tmp/pr2472-latest-dev-fcntl14-vma-opt-r{2,3}-20260923.log`。三轮均未修改原版 5000 次循环和 38 秒门槛；新 head 仍须核对远端 CI，不能把定向结果外推为完整系统套件通过。

三个旧 C 测试按本次用户决定清理，不再保留原程序。x86_64 的 `fcntl14` 承接跨进程随机正长度锁竞争和 `F_GETLK` 持锁信息；其余架构仍只有正式集合中的 `fcntl17`、`fcntl15`、`fcntl34`、`fcntl36` 提供路径级部分覆盖。逐项未承接断言记录在 `migration.csv`：

- `bug-fcntl-len-negative`：负 `l_len` 区间归一化、负长度解锁、`INT64_MIN`、负起点 `EINVAL`；
- `bug-fcntl-posix-lock`：固定 `SEEK_SET` 非阻塞冲突、父解锁后子重试、固定读锁与读写冲突阶段；
- `bug-fcntl-whence`：`SEEK_CUR`/`SEEK_END`、非法 whence 的 `EINVAL`、`F_OFD_GETLK` 相对区间、FIFO 相对区间。

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

`02-lock-probe-x86_64.log` 与旧 CI 记录的是修复前失败：第一个变体曾在 38 秒内 0 TPASS、1 TBROK。旧任务栈缓存、子进程页表失效策略和 `F_GETLK` ABI 回写尝试已经撤回。当前实现由 `PageTableRef::walk_occupied_range()` 缩短稀疏页表扫描，由 `AddrSpace::clear_quiescent_contents()` 对已退休、无 CPU/页表使用者的 MM 一次性拆树并释放 `MappingSlot` 所有权。x86_64 原始 LTP 连续六轮均得到 2 TPASS；没有缩减 5000 次操作、放宽上游超时或降低通过门槛。四架构完整 system 套件均已通过，但其他三架构的正式集合没有执行 `fcntl14`，不能外推 x86_64 的该用例结果。

`fcntl16` 输出三段 TINFO PASSED，但固定源码没有 TPASS 成功报告，wrapper 以 `0 TPASS, expected at least 1` 返回 1。它实际包含部分解锁场景，纠正旧账本关于无部分释放覆盖的说法；但不能通过把 TINFO 当作 TPASS 接入。`bug-fcntl-partial-wake` 与 `bug-fcntl-setlkw-blocks` 保留，也不只选同一原程序的绿色 OFD 候选掩盖 POSIX 候选失败。

### 3.2 其他保留项

`bug-fcntl-fd-mode-ebadf`、`bug-fcntl-ofd-pid-einval`、`bug-fcntl-posix-exit-release`、`bug-flock-failed-upgrade` 没有在本批建立完整等效映射，保留原程序和具体输入。普通 fcntl 无效 FD、正长度区间或 flock 排他冲突，不能分别证明读写模式不匹配、非零 OFD PID、退出自动释放或失败升级丢弃原共享锁。`bug-fcntl-len-negative`、`bug-fcntl-posix-lock`、`bug-fcntl-whence` 属于例外：本次按用户明确决定清理，未承接断言逐项记录在 `migration.csv` 与 2.9 节，不代表这些行为已被等效覆盖。

### 3.3 独立问题跟踪

每个暂缓问题单独跟踪，共用失败候选关联同一议题，避免为两个原程序重复建单。IPv6 的选项状态与原生地址身份属于不同问题，分别登记；历史问题的议题明确注明尚未在最新 dev 复验。

| 问题 | 议题 | 保留的原程序 |
| --- | --- | --- |
| fcntl14 修复前超时 | [#2341](https://github.com/rcore-os/tgoskits/issues/2341) | x86_64 已恢复正式集合并完成六轮本地原始测试；四架构完整 system 已通过，当前 PR 新 head 的 CI 待确认；三个旧 C 程序按用户决定清理 |
| fcntl16 无 TPASS 完成报告 | [#2342](https://github.com/rcore-os/tgoskits/issues/2342) | bug-fcntl-partial-wake、bug-fcntl-setlkw-blocks |
| ext4 SEEK_HOLE 返回 EINVAL | [#2343](https://github.com/rcore-os/tgoskits/issues/2343) | bug-fallocate-zero-punch |
| fchmodat2 O_PATH 空路径返回 EBADF | [#2344](https://github.com/rcore-os/tgoskits/issues/2344) | bug-fchmodat2-flags |
| IPV6_V6ONLY 状态与绑定约束 | [#2345](https://github.com/rcore-os/tgoskits/issues/2345) | bug-af-inet6-v4mapped |
| 原生 ::1 端点身份丢失 | [#2346](https://github.com/rcore-os/tgoskits/issues/2346) | bug-af-inet6-v4mapped |

其他议题未因本次修改而关闭；#2341 在当前 PR 的 CI 终态核实前保持开放。后续新增暂缓项仍须记录复现输入、失败证据与对应议题，不以原程序保留代替问题跟踪。

## 4. 验证与兼容性

本 PR 除测试清理外还包含 x86_64 的 `fcntl14` 接入和地址空间/页表性能修复。任务栈缓存和 `F_GETLK` ABI 回写等旧尝试未重新引入；退休 MM 的整树释放只在生命周期门禁确认不可再次激活后生效，不改变普通 `munmap` 和未发布 loader 回滚。兼容性结论限定到新接入用例实际证明的输入；原程序未被承接的断言不因同名 syscall 就视为继续覆盖。

### 4.1 验证入口

本 PR 重基后曾通过 `cargo xtask starry test qemu --arch x86_64 -c qemu/system/ltp-syscalls` 运行正式累计 LTP，再以四架构的 `-c qemu/system` 执行完整系统套件。该阶段 x86_64 正式 LTP 与完整套件均包含 `fcntl14` 的两种原始 5000 次操作变体，各得 2 TPASS；完整 system 计数分别为 x86_64 511/511、aarch64 484/484 加 perf 23/23、riscv64 507/507、loongarch64 507/507。日志为 `/tmp/pr2472-rebased-ltp-x86_64-20260923.log` 与 `/tmp/pr2472-rebased-system-<arch>-20260923.log`；另有六轮单独回归 `/tmp/pr2472-retired-no-preflight-fcntl14-r{1..6}-20260923.log`。但提交 `c0b8173e28` 的 x86_64 CI 与后续高负载本地执行仍触发超时，这些早期本地通过不能证明稳定通过。`fcntl14` 仅在 `cases-x86_64.txt`，以 `minimum-passes.txt` 的 2 TPASS 为完成契约；最新修复仍需新 head 的 CI 终态验证。

### 4.2 syscall 对照

固定 Linux 基准与 Starry 状态所有者分别列出。测试替换不改变 syscall 实现，不能把本表中的局部结果外推为完整锁生命周期、超时、用户页缺页或所有错误组合兼容性。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| fcntl(F_SETLK/F_SETLKW/F_GETLK) / x86_64:72；其他三架构:25 | [Linux v7.1 posix_locks_deadlock](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/locks.c#L1101) | POSIX 等待环返回 EDEADLK，查询持锁者和区间 | sys_fcntl → dispatch_fcntl → fcntl_setlk/getlk → PosixLockWaitGuard 与 FCNTL_LOCKS，进程身份所有权 | 无法确认 | fcntl17 四架构候选通过；x86_64 的原版 fcntl14 在最新 dev 上三轮定向 2 TPASS，但前一 head 的 x86_64 CI 超时，新 head 尚待验证；fcntl16 仍暂缓，其他架构未接入 fcntl14 |
| fcntl(F_OFD_SETLKW/F_OFD_SETLK) / x86_64:72；其他三架构:25 | [Linux v7.1 fcntl_setlk](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/locks.c#L2506) | 独立 OFD 的读写互斥及与 POSIX 锁的竞争，解锁后可继续访问 | sys_fcntl → dispatch_fcntl → fcntl_setlk → FCNTL_LOCKS 的 OFD/POSIX 所有者及 inode 等待队列 | 正确 | fcntl34/36；四架构候选验证通过，不包含 OFD GETLK 或最后关闭语义 |
| close / x86_64:3；其他三架构:57 | [Linux v7.1 filp_flush](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c#L1456) | 关闭同 inode FD 释放当前所有者记录锁，保留其他进程的锁 | sys_close → close_file_like → release_locks_on_close → release_inode_posix_locks | 正确 | fcntl15 的 dup/open/fork 三种组合，四架构验证通过 |
| flock / x86_64:73；其他三架构:32 | [Linux v7.1 flock](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/locks.c#L2214) | OFD 间共享/排他冲突、非阻塞 EWOULDBLOCK、阻塞信号中断 EINTR | sys_flock → flock_op → try_flock_once → FLOCK_LOCKS 及 inode 等待队列 | 正确 | flock02/04/06/07，四架构验证通过 |
| futex / x86_64:202；其他三架构:98 | [Linux v7.1 do_futex](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/futex/syscalls.c#L112) | 私有 WAIT 返回0，WAKE 的计数为1 | sys_futex → FutexContext::resolve → wait_nofault_until/wake，进程私有键及桶队列 | 正确 | 复用 futex_wait03，四架构验证通过 |
| getcwd / x86_64:79；其他三架构:17 | [Linux v7.1 getcwd](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/d_path.c#L413) | 缓冲区过短先 ERANGE，足够长但地址无效 EFAULT；返回当前路径 | sys_getcwd → current_fs_context 当前目录 → absolute_path → vm_write_slice | 正确 | getcwd01/02，四架构验证通过；不证明 raw 成功长度 |

## 5. 全量续迁

2026-09-14 从 `51c2d5077938e535ea9b8a5ec468f33dbd3de765` 开始处理当前 `qemu/system` 中全部 139 个 `bugfix-*`、154 个 `syscall-*` 目录，共 295 个原程序。初始源码清单保存在本轮证据目录的 `inventory.json`，后续删除不改变清单范围。本节记录新的执行规则，不改变上文历史批次的事实。LTP 仍固定为 `20260529`、提交 `3a64d78f58bdceba93ed321e91215fb969a047ed`，本机上游源码检出干净。

### 5.1 逐项处理

按原目录名称先处理 bugfix，再处理 syscall。原程序只有在对应上游用例完成全部适用架构后才清理；LTP 部分承接时，未承接的断言写入 `migration.csv`，不补写自定义用例。完全没有对应项的程序保留，候选失败则恢复正式清单、保留原程序并继续下一项，不修改内核、上游测试、超时或通过门槛。本轮完成全部候选处置及必要验证后提交、推送并创建拉取请求；不创建缺陷议题。

`cases.txt` 保存正式共同集合；探测时临时选择当前候选，结束后恢复正式集合。相同 LTP 用例的本轮结果可复用于多个原程序，失败结果也复用，不反复探测直到通过。架构范围只依据原源码限定，不能依据失败缩减。`syscall-test-vectored-io` 的三个可执行程序必须分别处置，不因主程序迁移而删除其余程序。

全部 295 个原程序已逐项处置：137 个部分替代并清理、69 个无对应项保留、89 个失败跳过（保留原测试）。其中 134 行的 `commit_subject` 为 `test(starry): migrate bugfix and syscall probes to LTP`；另外三个 fcntl 程序按用户决定清理，原提交主题为 `test(starry): drop fcntl bugfix C tests`。x86_64 后续恢复 `fcntl14` 才形成其随机锁场景的部分替代，其他架构仍仅有既有锁用例的路径覆盖；未承接断言保留在 `migration.csv` 与 2.9 节。其他行保留历史批次或范围外状态，不计入上述数量。混合目录保留 `test-special-fd-write-precedence`，仅删除已通过迁移的两个程序及它们的构建条目。

### 5.2 本轮证据

逐项命令、退出状态和失败输出写入 `migration.csv`，完整日志保存在实施机器 `/tmp/starry-ltp-5fb8-evidence/`。每个候选日志使用 `<LTP-ID>-<arch>.log`，历史日志不计入本轮通过证据。完成数量来自固定上游源码，并由 `minimum-passes.txt` 与现有 wrapper 检查；`TCONF`、`TBROK`、`TFAIL` 和零通过数均不能接入。

逐项处置及最终四架构累计 LTP、完整 system 验证均已完成。初始 x86_64 累计 LTP 基线已通过，见 `baseline-x86_64.log`。正式集合为 172 个共同用例，x86_64 另有 3 个专有用例；每个架构还有两个独立计数的 native 隔离回归。

`mprotect04` 在 aarch64 持续报告同一虚拟地址的页表映射冲突，未完成首条断言；本轮主动终止挂起的 QEMU 及同次 xtask，记录退出码 `-15`，不是自然超时。原程序保留，未重试；处置说明保存在 `mprotect04-abort.json`。

宿主派生 rootfs 镜像缓存累积导致空间耗尽：`mq_notify01` 的 riscv64 镜像准备失败，退出 1；随后 `msgctl12` 的 x86_64 日志和退出码未能落盘，退出码记为未知。两项按环境失败或证据缺失跳过，不据此认定内核行为错误，也不重试。相关说明见 `mq_notify01-environment.json`、`msgctl12-environment.json` 与 `progress-172-285.log`。清理本工作树 275 份旧派生镜像后释放约 608 GiB，保留源码、原始 rootfs 和运行日志，再继续其余候选。

最终整合基准为 `origin/dev 4a398bd618a5ad74b624d5668c3101d92a675839`，保留其新增 `nanosleep02` 及 `clock_nanosleep04` 完成门槛。重基前的累计验证仅作为阶段证据，日志统一为 `pre-rebase-*.log`；其中 aarch64 普通 system 分组通过，独立 system-perf 分组的 `perf-hw-sliced-period` 返回 1，协调器停止后未捕获该整条命令的退出码。重基后使用新的 `final-*.log` 和 `final-runs.json` 重新记录四架构结果。

### 5.3 最终验证

在整合基准上串行执行四架构定向 LTP，再执行四架构完整 `qemu/system`，八条命令退出码均为 0。定向计数包含两个 native 隔离回归；aarch64 完整套件分为普通 system 480 项和独立 system-perf 23 项，两组均通过。

| 架构 | 定向 LTP 分组 | 完整 system | 日志文件 |
| --- | --- | --- | --- |
| x86_64 | 177/177 | 506/506 | `final-ltp-x86_64.log`、`final-system-x86_64.log` |
| aarch64 | 174/174 | 480/480 + 23/23 | `final-ltp-aarch64.log`、`final-system-aarch64.log` |
| riscv64 | 174/174 | 503/503 | `final-ltp-riscv64.log`、`final-system-riscv64.log` |
| loongarch64 | 174/174 | 503/503 | `final-ltp-loongarch64.log`、`final-system-loongarch64.log` |

上述文件均位于本轮证据目录。`final-runs.json` 保存完整命令、退出码及耗时；`final-ltp-audit.json` 核对每个用例的最少 TPASS、16 条文件系统阶段契约和零失败标记。用现有 `generate-common.sh` 从四份新日志生成公共清单，与正式 `cases.txt` 逐字节一致。

`final-system-audit.json` 确认完整运行没有重复程序、已删除原程序残留或普通安装目录中的保留项遗漏。本轮保留的 161 个原程序中，155 个在普通套件实际执行，另 6 个维持既有 `starry-known-fail` 安装位置，不属于默认 system 执行集合。loongarch64 的 termios 测试在成功标记前输出 NUL 字节，离线核对解析时仅去除该前导字节，不修改原日志或运行器判定。

`integrity-audit.json` 核对全部 295 条本轮处置、293 个原目录和 466 个删除文件的初始源码哈希；保留源文件、混合目录构建引用和正式清单一致。`git diff --check` 与文档站点 `npm run build` 通过。最终累计验证没有失败，不需要撤销已通过迁移；重基前的 aarch64 perf 失败保留为阶段记录，本次新基线完整验证通过。
