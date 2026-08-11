# TGOSKits Day3 进度记录：设备隔离复现与干净双 Guest 基线

> 日期：2026-08-07
> 范围：赛题 1；QEMU AArch64、AxVisor、Linux 2-vCPU、Zephyr 1-vCPU

## 1. 今日结论

Day3 的设备隔离目标已完成：将 Zephyr 和 Linux 都从整棵 Host FDT 直通改为只使用 UART、GIC 和 timer 后，双 Guest 可以同时启动，最终日志中 `failed to release coherent DMA allocation; allocation quarantined` 为 0。

本日没有提交 AxVisor 实时路径代码改造。原因是当前 Zephyr 镜像是 `tests/benchmarks/latency_measure`，只输出一次 microbenchmark 汇总，不是 10 ms 周期任务；没有真实周期样本时改调度、timer 或 vIRQ 会失去可验证依据。实时改造和 A/B 测量顺延到 Day4/Day5。

## 2. 复现与定位过程

| 实验 | 结果 |
| --- | --- |
| 原双 Guest（Linux/Zephyr 均使用 `/` 直通） | VM1/VM2 启动成功，但约 193 条 DMA quarantine |
| 仅 Linux（仍使用 NVMe 直通） | 仍约 193 条，排除“两个 Guest 重复直通”是唯一原因 |
| Linux memory + initramfs、Zephyr 最小设备集（首次） | DMA quarantine 消失；Linux 因 `MAP_IDENTICAL` 下 ramdisk 固定 GPA 无法翻译而失败 |
| Linux memory + initramfs 改为 `MAP_ALLOC`、Zephyr 最小设备集 | VM1/VM2 boot success，Zephyr benchmark `PROJECT EXECUTION SUCCESSFUL`，DMA quarantine 为 0 |

关键根因不是简单的“把 quarantine 改成 free”。DMA API 在无法证明回收安全时主动 quarantine；本日通过不把 NVMe/PCIe 交给 Guest，绕开 Host block runtime 与 Guest passthrough 的 DMA 生命周期冲突。

## 3. 最终实验配置

Linux 实验配置：`os/axvisor/tmp/vmconfigs/day3-linux-initramfs-aarch64.toml`（该目录被 `.gitignore` 忽略，仅作为本机实验配置）。

- 2 vCPU，绑定 pCPU `[1, 2]`；GPA `0x8000_0000`，256 MiB；
- 内存内核 + 内存 initramfs；
- `MAP_ALLOC`（map type `0`），避免 ramdisk 地址与实际 Host allocation 脱节；
- 只直通 `/pl011@9000000`、`/intc@8000000`、`/timer`，不直通 PCIe/NVMe。

Zephyr 实验配置：`os/axvisor/tmp/vmconfigs/day3-zephyr-minimal-aarch64.toml`。

- 1 vCPU，绑定 pCPU `[3]`；GPA `0x4000_0000`，128 MiB；
- 只直通 UART、GIC、timer；
- 保留 Zephyr 自带 `latency_measure` microbenchmark。

Host FDT 由 QEMU `dumpdtb` 确认包含 `/pl011@9000000`、`/intc@8000000`、`/timer`、`/pcie@10000000`。最终配置没有把 `/pcie@10000000` 交给任一 Guest。

## 4. 最终运行证据

复现实验命令：

```bash
export PATH="$HOME/.local/qemu-10.2.1/bin:$PATH"
export PKG_CONFIG_PATH="$HOME/.local/dev-sysroot/usr/lib/x86_64-linux-gnu/pkgconfig"
export PKG_CONFIG_SYSROOT_DIR="$HOME/.local/dev-sysroot"
env -u RUSTFLAGS -u CARGO_ENCODED_RUSTFLAGS \
  cargo xtask axvisor qemu \
  --config os/axvisor/configs/board/qemu-aarch64.toml \
  --qemu-config os/axvisor/configs/qemu/qemu-aarch64.toml \
  --vmconfigs os/axvisor/tmp/vmconfigs/day3-linux-initramfs-aarch64.toml \
  --vmconfigs os/axvisor/tmp/vmconfigs/day3-zephyr-minimal-aarch64.toml \
  --rootfs tmp/axbuild/rootfs/rootfs-aarch64-alpine.img/rootfs-aarch64-alpine.img
```

`/tmp/day3-dual-isolated-v3.log` 中可核对：

```text
VM[1] boot success
VM[2] boot success
*** Booting Zephyr OS ... ***
PROJECT EXECUTION SUCCESSFUL
```

该日志 SHA-256：

```text
4534f7c673771897b929a30a0ce677e0c8579db0123c8ce0b32a7f9b7c5b3f74
```

最终日志没有 `quarantined` 或 `Failed to initialize guest VM`。单 Zephyr 复测日志为 `/tmp/day3-zephyr-minimal.log`，同样无 quarantine，并通过 `PROJECT EXECUTION SUCCESSFUL`。

## 5. 当前基线与未完成项

已有的 Zephyr microbenchmark 对照沿用 Day2 记录：原生 QEMU、AxVisor 单 Guest、AxVisor 双 Guest 均有 thread/FIFO/semaphore/mutex/heap 的平均耗时，但它们不是周期 jitter。

因此以下项目尚未声称完成：

1. 10 ms 周期任务的 `sequence,timestamp_ns,deadline_ns,actual_ns,jitter_ns` 原始 CSV；
2. Linux idle 与 CPU stress 下的 p99/p99.9/max 和 deadline miss；
3. 有数据支撑的 AxVisor 实时路径代码改造及改造前后 A/B。

这符合 Day3 的停止规则：先消除 DMA 噪声，再获取真实周期程序；不在没有样本的情况下修改调度、timer、vIRQ 或锁。

## 6. Day4 入口

1. 保持本日最终设备隔离配置，先获得可构建的 Zephyr 周期采样镜像；
2. 采集单 Guest、双 Guest idle、Linux stress 三组 CSV；
3. 依据 p99.9/max 冻结一个最小 AxVisor 改造点，先写旧实现必失败的确定性回归测试；
4. 代码改动后立即运行目标 crate 的 `cargo fmt` 和 `cargo xtask clippy --package <crate>`，再做短 A/B。
