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

## 3. 系统调用兼容性对照

结论仅针对本轮明确检查的路径，不表示整个系统调用在所有输入下都兼容。移除 procfs 或 sysfs 的特定断言后，LTP 的绿色结果不能证明那些文件的表示仍然正确。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| sched_getaffinity / x86_64:204；aarch64、riscv64、loongarch64:123 | [Linux v7.1 syscalls.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/sched/syscalls.c#L1278) | 本轮覆盖非空且不超过配置 CPU 数的掩码；坏地址 EFAULT、零长度 EINVAL、不存在进程 ESRCH | `sys_sched_getaffinity` → `scheduler_thread_id` / `scheduler_task` → 当前 PID namespace 的 `PidView` → ax-task `thread_affinity` 读取线程的 affinity → `vm_write_slice` | 正确 | `sched_getaffinity01`：四架构各四项 TPASS，LTP 阶段均成功，共同集生成与逐字比较成功；结论限本行四项行为 |
| sched_setaffinity / x86_64:203；aarch64、riscv64、loongarch64:122 | [Linux v7.1 syscalls.c](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/sched/syscalls.c#L1197) | 设置当前或指定线程的 CPU 亲和性；权限检查、用户掩码与允许 CPU 交集影响结果 | `sys_sched_setaffinity` → `check_sched_permission` → `vm_load` → ax-task `set_current_thread_affinity` 或 `set_thread_affinity_and_wait` | 无法确认 | 首项 `sched_getaffinity01` 不验证设置操作；清理时不宣称原 CPU1 设置断言已承接 |
