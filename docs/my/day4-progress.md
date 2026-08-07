# TGOSKits Day4 进度记录：Zephyr 周期基线与 Linux 压测证据审计

> 日期：2026-08-07
> 范围：赛题 1；QEMU AArch64、AxVisor、Linux 2-vCPU、Zephyr 1-vCPU

## 1. 今日结论

Day4 完成了可复现的 Zephyr 10 ms 周期采样器，并获得两个有效基线：原生 QEMU 和 AxVisor 单 Guest。双 Guest 场景中的 Zephyr 也完成 300 个样本，但 Linux 2-vCPU 是否真正进入内核没有串口证据，因此不能把它们命名为 Linux idle 或 Linux stress 结果。

今天没有修改 AxVisor 核心代码。现有数据没有把问题定位到 timer、vIRQ、调度器或锁临界区；在证据不足时不提交猜测性实时改造。

## 2. 采样器

正式源码位于 `scripts/test/zephyr-periodic/`，包括 CMake 工程、`prj.conf`、采样程序、构建说明和 AxVisor AArch64 配置模板。

- 周期：10 ms；样本数：300；
- 使用绝对 tick deadline；
- 输出 `sequence,timestamp_ns,deadline_ns,actual_ns,jitter_ns`；
- AxVisor memory image 必须使用 `zephyr.bin`，不能把 ELF 当作 raw image；
- Zephyr 版本：`aa37fa1ebc92`（4.4.99）；构建工具链和 GCC 兼容性 workaround 记录在采样器 README。

## 3. 有效基线

统计命令：

```bash
python3 scripts/test/rt_latency_stats.py results/day4/native.csv
python3 scripts/test/rt_latency_stats.py results/day4/axvisor-zephyr-single.csv
```

| 场景 | 样本 | mean jitter | p99 | p99.9 / max |
| --- | ---: | ---: | ---: | ---: |
| Native QEMU | 300 | 168.238 µs | 292.576 µs | 345.600 µs |
| AxVisor + Zephyr single | 300 | 335.484 µs | 606.416 µs | 882.480 µs |

两组样本的 `deadline_misses=300`。这是当前采样器的严格绝对 deadline 口径，不通过放宽阈值美化结果。

## 4. Linux 证据审计

原始双 Guest 运行日志：

- idle：`/tmp/day4-dual-idle-final2.log`；
- stress 初次尝试：`/tmp/day4-dual-stress-final.log`；
- 修正 initramfs 后：`/tmp/day4-dual-stress-confirmed.log`；
- `fs + MAP_ALLOC` 对照：`/tmp/day4-dual-stress-fs-map-alloc.log`；
- 官方 Linux 镜像对照：`/tmp/day4-dual-stress-official-kernel.log`。

验证结果：

1. 原生 QEMU 能执行 `/stress-init` 并输出 `LINUX STRESS START workers=2`，证明 stress initramfs 程序有效。
2. AxVisor 双 Guest 日志能看到 `VM[1] boot success`、`VM[2] boot success` 和 Zephyr 完成标记，但没有 `Booting Linux`、`Linux version` 或 `LINUX STRESS START`。
3. `fs + MAP_ALLOC` 能启动 VM task，但产生 DMA quarantine；因此不能作为干净基线。
4. `memory + MAP_ALLOC` 使用重编 Linux 和官方 Linux 镜像都没有 Linux 内核串口证据。

因此 `results/day4/axvisor-dual-idle.csv` 和 `axvisor-dual-stress-unverified.csv` 仅保留为观察数据，结果状态写在 `results/day4/README.md`，不支持 Linux idle/stress 的正式性能结论。

## 5. 目前未解决的问题

- AxVisor AArch64 下 `MAP_ALLOC` Linux Guest 没有可观察的内核启动；
- `fs + MAP_ALLOC` 的 DMA quarantine 仍未解决；
- 尚无 Linux CPU stress 下的有效 Zephyr A/B 数据；
- 尚无足够证据选择并改造 AxVisor 实时核心路径。

下一步应先修复或解释 Linux Guest 启动/观测路径，再继续 Day5 的完整 A/B；不应直接合并大范围上游 timer 或 scheduler 重构。

## 6. 证据哈希

有效结果：

```text
e44592961dc0a871e80af694edde77d22dc073f56e15ab7bd6337b5d1287b571  results/day4/native.csv
7ef3a9baea81934ee11ce2843acee1c3900788bd7e6b48351a187ab2be441e8f  results/day4/axvisor-zephyr-single.csv
6800b4f1800ad66a1beeb7ed8097010503f67ec0813edb4cb5109a3a297634a5  results/day4/axvisor-dual-idle.csv
```

未确认 stress 结果保留在 `results/day4/axvisor-dual-stress-unverified.csv`，但没有把它的哈希写入正式证据表；文件内容仍需按 `README.md` 的状态解释，它不是 Linux stress 的通过证据。
