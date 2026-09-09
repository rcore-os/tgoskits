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
