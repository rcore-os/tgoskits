# 多核编译模拟基准

`compile-sim-bench` 用固定进程依赖图近似 `cargo build -jN` 的调度形态，并让同一个静态 ELF 在
Starry 当前分支、Starry `dev` 与 Linux v7.1 PREEMPT_RT 中运行。它不下载源码或工具链，也不把
真实编译器版本、网络状态和增量缓存混入调度结果；LTP `hackbench` 仍作为高频 IPC 下界单独运行。
2026-09-01 的三系统正式对比见 `RESULTS-2026-09-01.md`。

## 1. 负载模型

基准把输入、依赖、每个节点的计算量和输出大小固定下来。`compile-sim-bench.c` 中的
`build_graph()` 建立满二叉依赖图，`run_build()` 只在依赖节点完成后把父节点加入 ready queue，
并用 `jobs` 限制同时存活的 worker 数量。

### 1.1 编译节点

每个节点都通过 `fork()` 和 `execv()` 启动同一静态程序的 `--worker` 模式。叶子读取固定 source
文件，非叶子读取两个依赖产物；worker 随后执行固定工作集上的数据依赖变换，并写出供父节点读取的
产物。根产物的 FNV-1a 校验和必须在所有轮次和并行度之间一致，否则基准失败。

默认 benchmark 使用 16 个叶子、31 个进程节点、每个叶子 256 KiB 输入、每个 worker 4 MiB
工作集、8 次变换和 128 KiB 输出。它覆盖多进程 fan-out/fan-in、CPU burst、内存工作集、页缓存、
文件读写、`fork/exec` 和依赖同步，但不模拟 `rustc` 前端、LLVM 或链接器的具体算法。

### 1.2 并行协议

QEMU 始终启动 4 个 online vCPU。`benchmark_main()` 先保存 4-CPU allowed mask，再分别限制整个
进程树到前 1 个或前 4 个 CPU；子进程继承该 affinity。这样测得的是相同 SMP 内核上的
`jobs=1` 与 `jobs=4`，不会把 UP/SMP 构建差异混入扩展性结果。

每种并行度先预热一次，再交错执行 5 轮正式样本。输出包含 `COMPILE_SIM_TOPOLOGY`、
`COMPILE_SIM_CONFIG`、`COMPILE_SIM_SAMPLE`、`COMPILE_SIM_RESULT` 和
`COMPILE_SIM_SPEEDUP`；最后必须出现唯一的 `COMPILE_SIM_BENCH_PASSED`。主要指标是 wall time
中位数和 `jobs=1 median / jobs=4 median`，不设置最低 speedup 门槛。

## 2. Starry 运行

`prebuild.sh` 使用 x86_64 musl 工具链构建静态 ELF，并把它与仓库已有的未修改 LTP `hackbench`
runner 一同写入 managed Alpine rootfs。两个 workload 因而使用相同 rootfs、Q35、TCG multi-thread、
512 MiB 内存、4 vCPU 和 NVMe snapshot 参数，但分别在独立启动中采样。

### 2.1 快速验证

默认配置把依赖图和工作量缩小，只验证拓扑、affinity、进程 DAG、产物校验和与结果解析。它不是正式
性能数据，运行命令为：

```bash
cargo xtask starry app qemu \
  -t qemu/compile-sim-bench \
  --arch x86_64
```

成功运行会输出两种并行度的预热和单轮样本，并以 `COMPILE_SIM_BENCH_PASSED` 结束。

### 2.2 正式采样

正式编译模拟必须显式选择 benchmark 配置，并保存完整串口输出。不要与其他 QEMU、编译或高 CPU
任务并发运行；不同分支也应在同一主机状态下交错采样。

```bash
set -o pipefail
cargo xtask starry app qemu \
  -t qemu/compile-sim-bench \
  --arch x86_64 \
  --qemu-config qemu-x86_64-benchmark.toml \
  2>&1 | tee target/compile-sim-bench.log
```

LTP 对照使用同一 app 生成的 rootfs，但把 guest 命令切换为现有 `ltp-hackbench-run benchmark`：

```bash
cargo xtask starry app qemu \
  -t qemu/compile-sim-bench \
  --arch x86_64 \
  --qemu-config qemu-x86_64-ltp-hackbench.toml
```

## 3. Linux 对照

Linux 对照必须使用同一份 app 生成的 rootfs snapshot，并保持 QEMU 机器、TCG、内存、vCPU、NVMe
和 workload 参数不变。区别只应是 `-kernel` 指向 Linux v7.1 PREEMPT_RT `bzImage`，以及内核命令行
通过 `init=` 选择本 app 安装的 PID 1 runner。

### 3.1 编译模拟入口

`linux-compile-sim-init.sh` 以 PID 1 启动正式编译模拟，转发其退出状态，打印
`LINUX_RT_COMPILE_SIM_PASSED` 后关机。Linux 必须启用 SMP、PREEMPT_RT、HRTICK、NVMe、ext4、
8250 串口与 QEMU 电源关闭所需能力。

内核命令行使用 `init=/usr/bin/linux-compile-sim-init`。串口日志中必须同时存在
`COMPILE_SIM_BENCH_PASSED` 和 `LINUX_RT_COMPILE_SIM_PASSED`；只看到启动 marker 不算完成。

### 3.2 LTP 入口

`linux-ltp-hackbench-init.sh` 运行 managed rootfs 中的原始 `/opt/ltp/testcases/bin/hackbench`，
具体 groups、loops、rounds、affinity 和结果解析均复用 `../ltp-hackbench/ltp-hackbench.sh`，避免
Linux 与 Starry 各自维护一套参数。

内核命令行使用 `init=/usr/bin/linux-ltp-hackbench-init`。正式完成要求同时出现
`LTP_HACKBENCH_APP_PASSED` 和 `LINUX_RT_LTP_HACKBENCH_PASSED`。`hackbench` 衡量 scheduler、
pipe IPC 与 wake/wait 吞吐，不应把它的 speedup 解释成完整项目编译 speedup。
