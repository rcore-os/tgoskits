# Linux 与 StarryOS 的 sysbench 板卡测量

这个应用同时提供 QEMU 功能验证和板卡测量，在同一块 OrangePi-5-Plus 上运行相同的 sysbench、动态库和测量工具，保留完整输出。它覆盖逐核绑核吞吐、内存首次接触、线程扩展、线程同步和顺序写内存。结果用于观察差异，不能单独证明差异来自调频、调度或内存管理。

sysbench 仅手动执行，不接入 CI。`apps/.ignore` 将它排除在应用批量发现和 `app qemu --all` 之外；显式指定 `-t sysbench` 仍可运行下面的 QEMU 与板卡命令。工具回归也按需手动执行。

## 1. 运行流程

`c/CMakeLists.txt` 经 `prepare_app_board_session_assets()` 复用板卡 C 资产流水线，生成一个 `share/sysbench.tar.gz` 会话文件。`c/prebuild.sh` 在隔离的 Alpine staging root 安装 sysbench；CMake 收集实际 ELF 依赖，拒绝引用宿主库。

### 1.1 StarryOS 测量

`init.sh` 在临时目录下载并解包文件，记录 SHA256，然后执行 `harness/run.sh board`。下载仅作有界重试，工作负载失败直接输出失败标记。应用使用项目 OrangePi 默认构建配置，当前为八核并启用网卡；无需在共享根文件系统安装软件。

```bash
cargo xtask starry app board -t sysbench -b OrangePi-5-Plus
```

`board-orangepi-5-plus.toml` 只有一个检查步骤，全部负载完成后才接受 `SYSBENCH_DONE`。租约结束时会话文件失效；临时文件在脚本退出时删除。

### 1.2 Linux 基线

`--linux-stage` 构建并上传相同资产，然后进入板卡常规 Linux。使用命令输出中的当前会话 URL，在串口执行下面的命令；保留租约直至测量结束。

```bash
cargo xtask starry app board -t sysbench --linux-stage
```

在 Linux 提示符运行下面的下载和测量命令，将 `SESSION_URL` 替换为本次打印的 `share/sysbench.tar.gz` URL。输出中的包哈希必须与 StarryOS 一致。

```sh
work=$(mktemp -d)
wget -O "$work/bundle.tar.gz" 'SESSION_URL'
export SYSBENCH_BUNDLE_SHA256=$(sha256sum "$work/bundle.tar.gz" | cut -d ' ' -f 1)
mkdir "$work/tools"
tar -xzf "$work/bundle.tar.gz" -C "$work/tools"
sh "$work/tools/run.sh" board > "$work/linux.log" 2>&1
cat "$work/linux.log"
```

测量结束后保存日志并删除该临时目录，再退出持有租约的宿主命令。工具不写调频 governor；比较时应另行记录板卡、电源、温度、后台负载、内核提交和调频策略。

## 2. 测量语义

`harness/run.sh` 维护 Linux 和 StarryOS 共用的参数与工作负载次序。CPU 使用 `--cpu-max-prime=20000`、每项三秒；线程数按允许 CPU 数量选择 1、2、4、8。同步与内存项使用脚本中明确的固定参数。

### 2.1 绑核与计时

`probe_common.h` 的 `pin_cpu()` 校验数字和允许掩码，检查 `sched_setaffinity()` 返回值，并在测量前后核对 `sched_getcpu()`。`cpuprobe --list` 返回当前允许的 CPU，不假设核编号连续或编号代表特定微架构。

`cpuprobe.c` 通过 `CLOCK_MONOTONIC` 测量固定整数循环。`membw.c` 使用编译器屏障保留每次内存操作，报告三个样本的中位数；`firsttouch_s` 包括两个缓冲区的首次写入，`memcpy_GBps` 按复制的有效字节计算，不代表总线读写总流量。所有无效输入或系统调用失败均返回非零状态。

### 2.2 日志比较

`harness/compare.py` 只接受完整的单次板卡日志，要求包哈希、参数、CPU 掩码和工作负载集合相同，并拒绝绑核不一致、缺失结果以及非有限数值。

```bash
python3 apps/starry/sysbench/harness/compare.py linux.log starry.log
```

输出的 `StarryOS/Linux` 是观测指标之比：吞吐越大越好，首次接触耗时越小越好。CPU 编号在两个内核间的物理映射仍需操作者确认；同一包与掩码不能证明环境相同。多次运行应分别保存并比较分布，单次数据不作为性能门禁。

## 3. 验证与历史

QEMU 功能验证使用同目录中的 `qemu-aarch64.toml` 和 `qemu-aarch64-matrix.toml`，通过相同的 C 资产流水线注入工具包。工具回归通过真实进程检查无效输入、受限 CPU 掩码和比较拒绝路径；共享运行器由 axbuild 标准库测试覆盖。

### 3.1 QEMU 功能验证

默认配置执行一次短 CPU 负载；矩阵配置执行 1、2、4 线程 CPU、线程同步、互斥锁和内存操作。QEMU 数据包含模拟器、宿主调度和虚拟设备开销，只作为功能验证。

```bash
cargo xtask starry app qemu -t sysbench --arch aarch64
cargo xtask starry app qemu -t sysbench --arch aarch64 --qemu-config qemu-aarch64-matrix.toml
```

### 3.2 工具回归

编译后的 `cpuprobe` 和 `membw` 可以在目标系统直接测试，或在宿主通过 qemu-user 执行交叉编译产物。`--runner` 省略时直接运行本机程序。

```bash
sh apps/starry/sysbench/tests/check.sh
python3 apps/starry/sysbench/tests/test_helpers.py --bin-dir /path/to/tools --runner qemu-aarch64
python3 apps/starry/sysbench/tests/test_compare.py
cargo xtask clippy --package axbuild
cargo xtask test --since origin/dev
```

### 3.3 原始实验

本分支继承 [原 PR #1658 的 head](https://github.com/rcore-os/tgoskits/tree/b8ef92916a65fc2a4acd9f198e47db3219098b3a/apps/starry/sysbench-board)。原提交保留了 2026 年 7 月日志、调频试验和报告，可用于追溯，但其中部分结果来自临时代码和不同核数配置。

本次整理移除了手工 SSH 部署、重复 Linux/Starry 脚本、实验启动配置以及根据吞吐反推核类型和 MHz 的分解模型。历史吞吐数值不作为当前实现的验证结果，也不据此计算互相独立的调频或调度优化倍数。
