# TGOSKits Day2 进度记录：RTOS、双 Guest 与实时基线

> 日期：2026-08-07
> 范围：赛题 1 的 Linux 2-vCPU、Zephyr RTOS、双 Guest 冒烟和第一版基线

## 1. 今日结论

Day2 的最小启动目标已经完成，RTOS 选择为 Zephyr：

```text
QEMU 4 pCPU
  └─ AxVisor
       ├─ VM[1] Linux：2 vCPU，绑定 pCPU 1/2
       └─ VM[2] Zephyr：1 vCPU，绑定 pCPU 3
```

已得到三类证据：

1. Linux 单 Guest 启动并输出 `SMP: Total of 2 processors activated`，随后进入 `/bin/sh`。
2. Zephyr 单 Guest 在 AxVisor 下输出 `*** Booting Zephyr OS ... ***` 和 `PROJECT EXECUTION SUCCESSFUL`。
3. Linux + Zephyr 同时启动；日志中有 `VM[1] boot success`、`VM[2] boot success`，并同时出现 Linux shell 和 Zephyr benchmark。

这仍然是“改造前”基线，不代表赛题要求的实时改造已经完成。

## 2. 与 Day2 验收门槛的对应关系

| 门槛 | 状态 | 证据 |
| --- | --- | --- |
| 选定并启动 FreeRTOS 或 Zephyr | 已完成 | Zephyr AxVisor 日志、benchmark 成功 |
| Linux 2-vCPU 启动 | 已完成 | Linux `SMP: Total of 2 processors activated` |
| Linux + RTOS 双 Guest 最小场景 | 已完成（带告警） | VM1/VM2 均 boot success；见第 5 节 |
| CPU、内存、设备、中断、镜像记录 | 已完成 | 第 4 节配置表和原始日志 |
| 统一延迟格式与统计脚本 | 已完成第一版 | `scripts/test/rt_latency_stats.py`；尚无周期任务长时间采样 |

## 3. 可复现实验命令

所有 AxVisor 命令都清除会覆盖 xtask 链接参数的 Rust flags，并补充开发 sysroot 的 host-side `libudev` 路径：

```bash
export PATH="$HOME/.local/qemu-10.2.1/bin:$PATH"
export PKG_CONFIG_PATH="$HOME/.local/dev-sysroot/usr/lib/x86_64-linux-gnu/pkgconfig"
export PKG_CONFIG_SYSROOT_DIR="$HOME/.local/dev-sysroot"

env -u RUSTFLAGS -u CARGO_ENCODED_RUSTFLAGS \
  cargo xtask axvisor qemu \
  --config os/axvisor/configs/board/qemu-aarch64.toml \
  --qemu-config os/axvisor/configs/qemu/qemu-aarch64.toml \
  --vmconfigs os/axvisor/tmp/vmconfigs/day2-linux-aarch64-smp2.toml \
  --rootfs tmp/axbuild/rootfs/rootfs-aarch64-alpine.img/rootfs-aarch64-alpine.img
```

Zephyr 单 Guest：

```bash
env -u RUSTFLAGS -u CARGO_ENCODED_RUSTFLAGS \
  cargo xtask axvisor qemu \
  --config os/axvisor/configs/board/qemu-aarch64.toml \
  --qemu-config os/axvisor/configs/qemu/qemu-aarch64.toml \
  --vmconfigs os/axvisor/tmp/vmconfigs/day2-zephyr-aarch64.toml \
  --rootfs tmp/axbuild/rootfs/rootfs-aarch64-alpine.img/rootfs-aarch64-alpine.img
```

双 Guest：

```bash
env -u RUSTFLAGS -u CARGO_ENCODED_RUSTFLAGS \
  cargo xtask axvisor qemu \
  --config os/axvisor/configs/board/qemu-aarch64.toml \
  --qemu-config os/axvisor/configs/qemu/qemu-aarch64.toml \
  --vmconfigs os/axvisor/tmp/vmconfigs/day2-linux-aarch64-smp2.toml \
  --vmconfigs os/axvisor/tmp/vmconfigs/day2-zephyr-aarch64.toml \
  --rootfs tmp/axbuild/rootfs/rootfs-aarch64-alpine.img/rootfs-aarch64-alpine.img
```

`os/axvisor/tmp/vmconfigs/` 当前被项目忽略，是实验性配置目录；正式提交前需要把绝对路径改成镜像准备流程可生成的相对路径。

## 4. 资源与镜像记录

| 项目 | Linux VM1 | Zephyr VM2 |
| --- | --- | --- |
| vCPU 数 | 2 | 1 |
| pCPU 绑定 | `[1, 2]` | `[3]`（单 Guest 时为 `[0]`） |
| Guest 内存 | GPA `0x8000_0000`，256 MiB | GPA `0x4000_0000`，128 MiB |
| 入口 | `0x8020_0000` | `0x4000_10a4` |
| 镜像 | `/guest/linux/linux-qemu`，来自本地 Alpine rootfs | `tmp/axbuild/rootfs/qemu-aarch64/zephyr/zephyr-qemu` |
| 设备模式 | passthrough，Linux 使用 NVMe 根盘 | passthrough；当前仍需进一步缩小设备集合 |
| Host QEMU | `qemu-system-aarch64`，QEMU 10.2.1 | 同左 |
| AxVisor 调度器 | 日志显示 `FIFO scheduler` | 同左 |

Host QEMU 使用 4 个 AArch64 `cortex-a72` vCPU，GICv3，8 GiB host RAM，NVMe rootfs。

## 5. 结果与问题

### 5.1 Linux 2-vCPU

`/tmp/day2-linux-smp2.log` 和双 Guest 日志均包含：

```text
SMP: Total of 2 processors activated.
Run /bin/sh as init process
```

这证明 CPU 数量已经由 Linux 客户机自身确认，不只是 VM 配置文件中的声明。

### 5.2 Zephyr 原生与 AxVisor 基线

原生 QEMU 日志：`/tmp/zephyr-native-elf.log`。AxVisor 单 Guest 日志：`/tmp/day2-zephyr.log`。

| Zephyr benchmark 项目 | 原生 QEMU | AxVisor 单 Guest | AxVisor 双 Guest |
| --- | ---: | ---: | ---: |
| thread create | 1295 ns | 1381 ns | 1463 ns |
| thread resume | 514 ns | 563 ns | 584 ns |
| semaphore take（阻塞/切换） | 553 ns | 681 ns | 688 ns |
| recursive mutex lock | 172 ns | 156 ns | 150 ns |
| heap malloc | 2411 ns | 2735 ns | 2792 ns |

这些是 Zephyr 自带的 microbenchmark min/avg/max 汇总中的平均值，不是周期任务 jitter；它们只能作为今天的 RTOS 原生/虚拟化初始对照，不能替代后续统一周期采样。

### 5.3 双 Guest 的 DMA 告警

双 Guest 启动和 benchmark 成功，但 `/tmp/day2-dual-linux-zephyr.log` 与排除 PCIe 的复测日志都出现约 193 条：

```text
failed to release coherent DMA allocation; allocation quarantined
```

当前两个 VM 都使用 `passthrough_devices = [["/"]]`，这会把整棵设备树重复直通，Linux 的 NVMe/PCI 和 Zephyr 的设备所有权边界不清楚。仅加入 `excluded_devices = [["/pcie@10000000"]]` 没有消除告警，说明还需要核对实际 FDT 节点路径和 DMA 生命周期。

因此，双 Guest 当前结论是：**启动功能通过，资源隔离不通过**。在进入压力实时测量前必须修复或明确这个告警，否则数据会被设备回收噪声污染。

### 5.4 FreeRTOS 交叉结果

FreeRTOS 原生 QEMU 可以运行自带 benchmark（`/tmp/freertos-native-elf.log`），但当前 AxVisor 临时配置下尚未得到稳定 Guest 串口证据：自动 DTB 路径出现 `sys_write => Err(EBADF)`，显式 QEMU DTB 路径出现 AArch64 异常。故 Day2 不把 FreeRTOS 作为主 RTOS，保留为后续对照。

## 6. 统一延迟日志格式

周期任务后续必须输出单调时钟的绝对值，避免只输出“耗时”而无法重算 deadline：

```text
sequence,timestamp_ns,deadline_ns,actual_ns,jitter_ns
0,1000000000,1001000000,1000998000, -2000
```

统计命令：

```bash
python3 scripts/test/rt_latency_stats.py results/zephyr-idle.csv
```

输出 mean、p99、p99.9、max 和 deadline miss 次数。后续采集脚本必须同时保存原始 CSV 和运行命令，不接受只提交汇总数字。

## 7. Day3 优先级（按收益排序）

1. **先修双 Guest 设备所有权**：明确 Linux 独占 NVMe，Zephyr 只保留 UART/GIC/timer；让双 Guest 在 0 条 DMA quarantine 下运行。
2. **采集改造前周期基线**：Zephyr 1 ms/10 ms 周期任务，Linux idle 与 stress 两组，至少先跑 30 秒；统计 jitter、调度/中断延迟和 max。
3. **冻结一个 AxVisor 主改造点**：当前代码日志明确使用 FIFO scheduler；结合数据决定是共享 pCPU 的抢占/固定优先级，还是 vIRQ pending 队列、IRQ affinity、timer rearm 路径。
4. **再改代码并做 A/B**：改造前后使用相同 VM 配置、CPU 绑定、QEMU 版本和 workload；修改 Rust 代码前先补会在旧实现失败的确定性回归测试。

## 8. 原始日志

- `/tmp/day2-linux-smp2.log`
- `/tmp/day2-zephyr.log`
- `/tmp/day2-dual-linux-zephyr-exclude-pcie.log`
- `/tmp/zephyr-native-elf.log`
- `/tmp/freertos-native-elf.log`

这些 `/tmp` 文件是本机原始证据；提交或迁移到其他环境时，应复制到带日期的实验归档目录，并在表格中记录 SHA-256。
