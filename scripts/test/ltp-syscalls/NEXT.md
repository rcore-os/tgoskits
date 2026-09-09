# Starry syscall 测试续迁记录

## 1. 范围与基准

本批从 `dev cc8faa9222` 开始，继续上一轮已合入的 PR #2322。LTP 固定为 `20260529`、提交 `3a64d78f58bdceba93ed321e91215fb969a047ed`；Linux 对照仍为 v7.1、提交 `8cd9520d35a6c38db6567e97dd93b1f11f185dc6`。本批不修改内核或复制上游测试，候选失败时保留原程序并记录暂缓原因。

### 1.1 执行与提交

`cases.txt` 是实际累计执行集合，`probe-cases.txt` 保存候选。探测期间临时选择候选集合，探测日志与最终累计验证分别保存；失败候选保留在候选清单，不进入最终执行集合。每个原程序只在对应 LTP 完成四架构验证后单独提交，共用用例只安装一次；提交主题写入 `migration.csv` 以免变基使账本 hash 失效。

### 1.2 覆盖边界

部分替代逐条列出未承接断言，不能将同一 syscall 的其他输入视为完整等效。IPv6、fallocate 和 fchmodat2 已记录的失败项本批不重新修复。`bug-fb0-offset` 直接检查 framebuffer 设备偏移，属于设备验证，保留。尚未完成等效审计或缺少通过候选的锁测试也保留，不因旧账本标注“待清理”就直接删除。

## 2. 逐项映射

每小节记录原程序的输入、生命周期断言和上游承接范围。上游源码链接固定到 LTP 提交；日志保存在实施机器 `/tmp/starry-ltp-next-evidence/`。

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
