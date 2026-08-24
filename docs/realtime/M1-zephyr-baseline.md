# M1 · Zephyr 改造前实时性基线（QEMU aarch64）

> 阶段 1 / M1 交付物。测量 AxVisor **改造前**承载 Zephyr 客户机时的内核原语延迟基线，建立空载 vs 压力对照，供后续 M2–M4 改造效果对比。
> 完成日期：2026-06-29。原始数据见 `tmp/m1/data/`，采集脚本见 `tmp/m1/*.sh`。

## 1. 测量对象与方法

**被测**：预编译 Zephyr 客户机镜像（`qemu-aarch64` bundle 内 `zephyr/zephyr-qemu`，build `b70c045e4cca`），运行 Zephyr `latency_measure` 基准——开机跑一次，打印 **43 项内核原语延迟**（线程操作、FIFO/LIFO、events、semaphore、condvar、stack、mutex、heap），每项给 cycles + ns，结束于 `PROJECT EXECUTION SUCCESSFUL`。

**为何用它**：本机无 Zephyr 构建链（无 west/SDK/交叉 gcc），无法自建 cyclictest 式周期任务 app。该基准是 Zephyr 官方实时性基准，其 **context-switch 类指标（`*.wake+ctx.k_to_k`、`*.blocking.k_to_k`）和 `thread.*`** 直接反映调度路径延迟，是 hypervisor 干扰最敏感的项。

**采样方式**：基准每次启动只产一组数 → **多次启动采样**（N=12）求 min/mean/max/std/jitter。脚本 `tmp/m1/run_zephyr_baseline.sh`：每次启动 AxVisor/QEMU → 等 `PROJECT EXECUTION SUCCESSFUL` → 提取 43 项 → 杀 qemu → 重复，输出 `run,metric,cycles,ns` CSV。

**对照**：
- **空载**：宿主无额外负载。
- **压力**：宿主 32 个 CPU 忙循环燃烧器（2× 超额订阅 16 核），使宿主调度器与 QEMU TCG 线程争抢。脚本 `tmp/m1/run_with_stress.sh`（带 EXIT 清理陷阱）。无 stress-ng，故用可移植忙循环等价负载。

## 2. 运行环境

| 项 | 值 |
|---|---|
| 平台 | QEMU 10.2.1，`-machine virt,virtualization=on,gic-version=3 -cpu cortex-a72 -smp 4 -m 8g` |
| 执行模式 | **TCG**（aarch64-on-x86，非 KVM） |
| 宿主 | 16 核，工具链 nightly-2026-05-28 |
| Zephyr vCPU | `cpu_num=1`，`phys_cpu_ids=[0]`（pCPU 0），中断 passthrough |
| AxVisor 调度 | ArceOS **FIFO** 调度器（日志：`use FIFO scheduler`） |

⚠️ **重要前提**：TCG 非实时环境，**绝对值不代表真实硬件**最坏延迟。本基线的价值在**相对对比**——空载 vs 压力、以及后续改造前 vs 改造后。真实硬件数据留待板卡阶段补充。

## 3. 结果

### 3.1 空载基线（N=12，单位 ns）

| 类别 | 代表指标 | mean | max | jitter(max-min) |
|---|---|---|---|---|
| 线程创建 | thread.create | 1505 | 1589 | 127 |
| 上下文切换 | semaphore.give.wake+ctx | 684 | 738 | 88 |
| 阻塞收取 | fifo.get.blocking | 626 | 650 | 57 |
| condvar | condvar.signal.wake+ctx | 1070 | 1134 | 101 |
| 堆分配 | heap.malloc | 2584 | 3352 | 1069 |

空载下 context-switch 类延迟稳定（mean 600–1100ns，jitter <130ns）；heap 类天然抖动最大。完整 43 项见 `tmp/m1/data/zephyr-idle.summary.txt`。

### 3.2 空载 vs 压力（worst-case 退化 = stress_max / idle_max）

压力下 **mean 膨胀约 2–11×，worst-case max 膨胀最高 ~89×**。退化最严重者：

| 指标 | 空载 max (ns) | 压力 max (ns) | ×max |
|---|---|---|---|
| semaphore.take.immediate | 138 | 12279 | **88.9** |
| heap.malloc.immediate | 3352 | 125335 | **37.4** |
| stack.pop.immediate | 326 | 9336 | 28.6 |
| lifo.get.blocking.k_to_k | 658 | 18279 | 27.8 |
| fifo.put.immediate | 237 | 6420 | 27.1 |
| semaphore.take.blocking.k_to_k | 605 | 14143 | 23.4 |
| events.wait_all.blocking.k_to_k | 705 | 14349 | 20.3 |
| thread.create | 1589 | 20397 | 12.8 |

完整对照见 `tmp/m1/data/idle-vs-stress.txt`。

## 4. 关键结论（作为改造靶点）

1. **空载已相当稳定**，问题在**负载下的最坏情况尾部**：单次 `heap.malloc` 最坏达 **125µs**（空载 ~3.4µs）。这正是实时系统关心的 worst-case latency。
2. **退化根因（假设，待 M2 验证）**：Zephyr vCPU 与宿主/QEMU 线程共享物理资源、AxVisor 用 **FIFO 非抢占式**调度、vCPU 未独占 pCPU。→ 对应任务一改造：**partition 调度 + RT vCPU 独占 pCPU + 固定优先级**。
3. **改造成效将以本基线为对照**：目标是显著压低**压力下的 max 与 jitter**（而非空载 mean）。

## 5. 复现

```bash
export PKG_CONFIG_PATH=/usr/lib/x86_64-linux-gnu/pkgconfig:$PKG_CONFIG_PATH
# 空载基线（N=12）
bash tmp/m1/run_zephyr_baseline.sh 12 idle
bash tmp/m1/aggregate.sh tmp/m1/data/zephyr-idle.csv
# 压力基线（N=12，32 燃烧器）
bash tmp/m1/run_with_stress.sh 12 32
# 对照
bash tmp/m1/compare.sh tmp/m1/data/zephyr-idle.csv tmp/m1/data/zephyr-stress.csv
```

## 6.5 多核 Linux 周期抖动基线（A1）

补齐任务一**主角**（多核 Linux 客户机）的周期任务抖动基线——这是竞赛明确要求的"周期任务抖动 / 最大延迟"指标。

**方法**：自写静态 musl cyclictest 探针 `tmp/m1/guest/rt_cyclictest.rs`（每在线 CPU 一个 `SCHED_FIFO` 线程，`sched_setaffinity` 钉核，`clock_nanosleep(TIMER_ABSTIME)` 绝对定时，测唤醒延迟 = 实际-期望），`rustc --target aarch64-unknown-linux-musl -C linker=rust-lld` 交叉编译为全静态二进制，`debugfs` 注入 rootfs，guest `init=/rt-init.sh` 自动运行并 `RTC_*` 输出到控制台。周期 1ms，每核 10000 次，共 2 核 20000 样本/次。注入与采集脚本 `tmp/m1/{guest/,run_linux_cyclictest.sh}`。

> 工具坑（已记录）：本机无 aarch64 交叉 gcc，且 rootfs 内 gcc 的 cc1 无法执行；改用 Rust 静态二进制。rootfs 用 debugfs 注入，反复写会损坏 ext4，须用干净副本 + 单次写入 + guest 关机前 `remount,ro`。

**结果（period=1ms，2 vCPU，各 10000 周期）**：

| 指标 | 空载 | 压力（32 燃烧器，loadavg≈32） | 退化 |
|---|---|---|---|
| min (µs) | 48.0 | 82.8 | 1.7× |
| avg (µs) | 95.5 | 2247.6 | **23.5×** |
| max (µs) | 2831.7 | 25438.1 | **9.0×** |
| p99 (µs) | ~300 | ≥4000（直方图饱和） | — |

**结论**：与 Zephyr 一致——空载已有可观抖动（TCG 所致），但**负载下 worst-case 周期延迟从 ~2.8ms 暴涨到 ~25ms、平均延迟 23× 恶化、p99 突破 4ms**。这是任务一改造（partition 调度 + RT vCPU 独占 pCPU）要直接压低的核心指标。原始数据 `tmp/m1/data/linux-cyclictest-{idle,stress}.txt`。

> 测量改进项：直方图上限 HMAX=4000µs 导致压力下 p99 饱和；改造后对比时应增大 HMAX 以获得精确 p99/p99.9。

## 6.6 复现（A1 Linux cyclictest）

```bash
export PKG_CONFIG_PATH=/usr/lib/x86_64-linux-gnu/pkgconfig:$PKG_CONFIG_PATH
# 1) 编译静态探针
rustc --edition 2021 -O --target aarch64-unknown-linux-musl -C target-feature=+crt-static \
  -C linker=rust-lld -C linker-flavor=ld.lld -o tmp/m1/guest/rtc tmp/m1/guest/rt_cyclictest.rs
# 2) 干净 rootfs + 单次注入 /rtc 和 /rt-init.sh（见 tmp/m1/guest/）
# 3) 跑空载与压力（脚本带 init=/rt-init.sh 的 qemu-config）
bash tmp/m1/run_linux_cyclictest.sh idle 0
bash tmp/m1/run_linux_cyclictest.sh stress 32
```

## 7. 局限与后续

- **TCG 非实时**：绝对值不可作 RT 保证；需板卡（OrangePi-5-Plus/RDK-S100）补真实硬件数据。
- **指标维度有限**：本镜像不测周期任务抖动 / 中断响应延迟。补全需：① 装 Zephyr SDK 自建 cyclictest 式 app；或 ② **hypervisor 侧打点**测 vCPU 调度延迟、IRQ 注入延迟（有 AxVisor 源码，更 RT 相关，建议在 M3 做）。
- **压力模型粗**：当前为宿主级 CPU 压力。更具代表性的是**多 VM 同核争抢**（Zephyr + Linux 压力 VM 同 pCPU 0），将在 M2/M4 改造对照时引入。
- **基线 Linux 侧**：M1 仅覆盖 Zephyr；任务一还需多核 Linux 客户机的周期抖动基线（Linux guest 有完整 userspace，可跑周期任务测量）。
