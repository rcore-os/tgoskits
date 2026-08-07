# TGOSKits Day3 计划：设备隔离与实时基线

> 日期：2026-08-07
> 前置提交：`1d9c18406 docs(progress): record day1 and day2 results`

## 今日只做三件事

1. 修正 Linux/Zephyr 双 Guest 的设备所有权，先消除 DMA quarantine。
2. 采集改造前的周期延迟基线。
3. 根据数据只选并实现一个 AxVisor 改造点，不同时改多个子系统。

## 1. 设备隔离（优先，约 1 小时）

### 操作

1. 用当前双 Guest 命令复现一次，并保存新日志。
2. 检查每个 VM 是否真正加载了 `excluded_devices`；日志必须出现对应路径，不能只看 TOML 文件。
3. 从 Host FDT/生成的 Guest FDT 中确认 UART、GIC、timer、PCIe/NVMe 的实际路径。
4. Linux 暂时保留 NVMe 所需设备；Zephyr 改成最小设备集合（UART、GIC、timer），不直通 PCIe/NVMe。
5. 重新运行双 Guest，观察至少 30 秒。

### 验收

```text
VM[1] boot success
VM[2] boot success
failed to release coherent DMA allocation = 0
```

如果仍有告警，记录“触发 VM、设备路径、数量、发生时间”，先暂停实时压力测试，不用猜测性修改。

## 2. 改造前实时基线（约 1.5 小时）

使用 Zephyr 的 10 ms 周期任务，输出已有统一格式：

```text
sequence,timestamp_ns,deadline_ns,actual_ns,jitter_ns
```

先跑 30 秒，数据稳定后再扩展到 5 分钟。只保留三组最有价值的场景：

| 场景 | 目的 |
| --- | --- |
| Zephyr 单 Guest、Linux 不运行 | 虚拟化自身基线 |
| Linux + Zephyr，Linux idle | 双 Guest 正常开销 |
| Linux + Zephyr，Linux CPU stress | 观察共享调度竞争 |

每组保存原始 CSV、启动命令和 QEMU/AxVisor 配置；用以下脚本统计：

```bash
python3 scripts/test/rt_latency_stats.py results/<scenario>.csv
```

记录 mean、p99、p99.9、max 和 deadline miss。

## 3. 冻结并实现一个改造点（约 2 小时）

选择标准按顺序排列：

1. 能由基线数据复现；
2. 影响实时路径，且改动边界小；
3. 能写确定性回归测试；
4. 能做改造前后 A/B 对比并快速回滚。

候选只保留一个：

- 若延迟随 Linux CPU stress 明显上升：优先检查 FIFO 调度/抢占路径；
- 若延迟与虚拟中断相关：检查 vIRQ pending 队列和锁临界区；
- 若延迟呈周期性跳变：检查 AxVM timer rearm 和跨 CPU 同步。

当天只改一个最小边界：先补一个旧实现必失败的确定性回归测试，再实现改动，最后跑同一组基线做快速 A/B。不要同时修改调度、timer、IRQ 和锁。

## Day5 的 80% 主线目标

Day3 完成任务一的最小闭环后，按下面节奏推进：

| 天数 | 必须交付 | 累计目标 |
| --- | --- | ---: |
| Day3 | 双 Guest 设备隔离、周期基线、AxVisor 一项实质改造和 A/B 结果 | 35% |
| Day4 | Linux ↔ Zephyr 双向 UDP/IP，`CONTROL/STATUS/ERROR/ACK` 最小协议 | 60% |
| Day5 | Linux 内真实小型 MLP 推理，控制参数经网络到 Zephyr，状态回传并展示 | 80% |

这里的“80%”指三个任务的可运行主线 MVP，不等同于最终评分 80 分；长稳测试、故障注入、完整文档和演示视频仍放在 Day6 之后。

## Day3 结束条件

- [x] 双 Guest 启动成功，DMA quarantine 为 0；
- [ ] 至少 3 组周期 CSV 和统计结果归档（当前 Zephyr 镜像不是周期任务，顺延到 Day4）；
- [ ] 一个主改造点已经落地，有回滚方案、回归测试和初步 A/B 结果（无真实周期数据，顺延到 Day4/Day5）；
- [x] 更新 `docs/my/day3-progress.md`；
- [ ] Day3 工作完成后再提交一次 commit。

## 已知风险

- 最终配置使用显式最小 passthrough 列表，因此 `Found excluded devices: []` 是预期结果，不代表把整棵 `/` 直通；
- Zephyr 的串口、timer 节点名称依赖实际 FDT，不能直接照抄地址；
- DMA 告警未清除前，实时数据不作为正式结论。
