# Task 1 两个缺口的闭环报告（2026-08-20）

本报告对应任务一评审中最容易丢分的两个问题：

1. Linux vCPU 与 RTOS vCPU 是否真的在同一个 pCPU 上竞争，以及实验收尾
   是否可靠；
2. IRQ 返回尾部抢占是否真正接入了 AxVisor 的内部关键路径，同时避免把
   每个普通 IRQ 变成一次无界调度。

## 给评审的直接结论（当前提交状态）

| 问题 | 当前判断 | 可以明确声称的范围 | 不能声称的范围 |
|---|---|---|---|
| 共核竞争与实验收尾 race | **在 QEMU 测试协议范围内已完整解决** | Linux 与 Zephyr 确实共享 pCPU；hold/release、显式 console 选择、marker 顺序和 QMP 正常退出均已闭环 | 不能把 QEMU TCG 的尾延迟当成物理板 WCET，也不能把旧失败运行当成当前结果 |
| IRQ 返回尾部抢占与 GIC completion 顺序 | **根因已解决，功能采用有界/条件化实现** | completion/EOI 先于 guard 释放；严格更高优先级唤醒可尾部抢占；同级 IRQ 风暴不再无界重调度 | 不是“每个 IRQ 都抢占”；尚未证明所有架构、所有 IRQ 源和物理板硬实时上界 |

因此，这两个扣分点不应再按“完全未修复”处理，但也不应写成“所有平台、所有
IRQ、所有最坏情况都已证明”。按当前可提交证据，Task 1 的合理自评为
**26–27/30**：27 分是完整机制和 QEMU 证据均被采纳时的保守上限，26 分是
官方更严格看待 upstream `dev` 可比运行和物理板缺口时的安全估计。

Task 1 目前进入**冻结/收口状态**：后续优先级放在最终文档与交付包、Task2
证据、StarryOS/STERRORS 可观察闭环，以及有条件的第二 RTOS/板卡；除非低成本
实验能直接补一项可比的最坏情况证据，否则不再扩大 Task 1 调度机制探索。

## 一、共核实验：从“能启动”到“可复现闭环”

### 拓扑和变量

```text
Linux vCPU0  ─┐
              ├── pCPU1（直接竞争）
Zephyr vCPU0 ─┘
Linux vCPU1 ───── pCPU2（Linux housekeeping）
```

`dedicated_cpus=none`，没有 host burner；RR、Fixed FIFO、FP-RR 只替换
AxVisor scheduler feature，Guest 镜像、内存、设备和 IRQ 路由保持不变。
这排除了“静态分区带来收益”的解释。

### 原失败原因

Linux cyclictest 完成后立即 `PSCI_SYSTEM_OFF`，而 Zephyr 在 QEMU TCG 下
可能尚未完成。runner 随后尝试重新 attach Linux，得到：

```text
VM[1] is not running
```

另一个隐蔽因素是旧的 `rt-linux-initramfs.cpio.gz` 没有重新打包，磁盘上的
脚本已经更新但 Guest 仍在运行旧逻辑。

### 修复

```text
Linux complete
→ RT_CYCLICTEST_HOLD_READY
→ runner 收集 Linux/Zephyr/VM-exit 证据
→ runner 发送 release
→ RT_CYCLICTEST_RELEASED
→ Linux PSCI_SYSTEM_OFF
```

runner 还改为在 Zephyr 完成后显式选择 `cmd vm console 1`，并在采样窗口
前、Zephyr 完成后、最终退出前保存三次 VM-exit snapshot。构建命令必须先
执行：

```bash
scripts/test/rt-partition/build-rt-tools.sh
```

### 当前闭环证据

| 策略 | 证据目录 | Linux avg / P99 / max | Zephyr P99 / max | 结果 |
|---|---|---:|---:|---|
| RR | `results/task1/irq-tail-priority-filter-rr-smoke-20260820/stress-guest-shared/` | 1,961 us / 7,142 us / 310,639 us | 31.582 ms / 67.259 ms | accepted |
| FP-RR | `results/task1/irq-tail-priority-filter-smoke-20260820/stress-guest-shared/` | 2,170 us / 19,549 us / 349,196 us | 35.632 ms / 145.725 ms | accepted |

两个目录均有 3000/3000 Zephyr 样本、Linux histogram、CPUStat、VM-exit
快照、hold/release marker 和正常 QMP 退出。数字受 TCG 运行时调度影响，
这里的核心结论是“同一 pCPU 的竞争确实发生且测试可收尾”，不是宣称每个
百分位都改善。

## 二、IRQ 尾部抢占：从失败反例到条件化实现

### 机制缺口

GIC 在 AxVM 中已经完成 acknowledge，原 host dispatch 直接调用动态 IRQ
框架，绕过了 AxHAL 的统一 IRQ-entry/preemption-release 边界。因此 IRQ
handler 唤醒高优先级 vCPU 后，VM-exit 返回点不一定消费 `need_resched`。

### 为什么不能“每个 IRQ 都抢占”

两版实验已经给出反证：

- completion 放在调度边界之后：Linux P99 撞到 20 ms histogram ceiling；
- completion 虽移入 guard，但每个 acknowledged IRQ 都允许尾部调度：Linux
  约 258 个样本后停滞，`slice_preserving_preemptions=78906`、
  `voluntary_requeues=6128290`。

根因是 timer/console IRQ 风暴产生同级 vCPU 的重复切换，而不是单纯的
“优先级算法不够快”。失败目录保留在
`results/task1/irq-tail-preemption-*-20260820/`。

### 当前实现

1. `axhal::irq::dispatch_acknowledged_irq` 接收已经 acknowledge 的
   `IrqId` 和 caller-owned completion closure；不二次 acknowledge。
2. dispatch 后先执行 GIC deactivate/EOI，再撤销 IRQ context、释放
   preemption guard，保持普通硬件 IRQ 的返回顺序。
3. AxVM GIC 只负责解析 token 并提供 completion；路由和所有权不变。
4. 固定优先级模式下，IRQ context 中只有“唤醒任务优先级严格高于当前任务”
   才设置 `need_resched`；CPU 正在 idle 时保留唤醒例外。RR/FIFO 路径保持
   原行为。

### 验证结果

```text
cargo check -p ax-hal -p ax-task -p axvm                 PASS
cargo test -p ax-sched                                   20 passed
cargo test -p ax-task --features test,smp,sched-prio-rr  56 passed
cargo test -p ax-hal --features axtest,host-test         4 passed
Python rt-partition tests                                 55 passed
真实 QEMU RR/FP-RR 共核 smoke                              均 accepted
```

真实 smoke 的共同 marker 顺序为：

```text
PERIODIC LATENCY COMPLETE samples=3000
RT_CYCLICTEST_COMPLETE
RT_CYCLICTEST_HOLD_READY
RT_CYCLICTEST_RELEASED
PSCI_SYSTEM_OFF
```

## 最终边界

这两个问题现在都已完成“根因解释 + 代码修复 + 真实运行验证 + 失败证据
归档”。可以提交的硬结论是：

- 共核拓扑是实际共享 pCPU，不是静态分区；
- Linux 收尾 race 已修复，结果采集有明确 hold/release 协议；
- acknowledged IRQ 的 completion 顺序已修正；
- IRQ 尾部抢占改为优先级条件化，旧的 Linux 停滞不再复现。

仍需保守表述：QEMU TCG 下 P99/P99.9 会有明显波动；没有据此声称所有
指标改善，也没有把 QEMU 结果等同于物理 RK3588 的最坏情况保证。

## 附录：问题考古与经验教训

### 时间线

1. 先做了独占/拓扑矩阵，发现“18 倍”主要由 pCPU 隔离贡献，不能作为
   调度机制收益。
2. 转向 Linux/Zephyr 同 pCPU 的共享实验，首次暴露 Linux 提前关机和
   console attach race。
3. 修复 hold/release 后，RR 与 FP-RR 都能完成完整采样和收尾。
4. 尝试无条件 IRQ-tail 抢占，先后出现 P99 撞上限和 Linux 258 样本停滞。
5. 将 GIC completion 放回 IRQ guard 内，并把尾部抢占限制为“严格更高优先级
   唤醒”，最终 smoke 闭环通过。

### 设计阶段的根因

早期方案把三个正交变量混在一起：CPU 拓扑、scheduler policy、实验收尾
协议。这样即使数字变好，也无法知道收益来自哪一层；而 IRQ 方案又把“需要
高优先级抢占”误化成“每个 IRQ 都应立即调度”。本轮把三者拆开，并明确
`completion → IRQ-context withdrawal → preemption release` 的顺序约束。

### 实现阶段的局部优化陷阱

只看平均值或 P99 会掩盖“最大延迟几十秒”和“Guest 已停止但 runner 还在
收集”的端到端故障。调度计数显示，无条件尾部抢占增加的是切换/回队列次数，
并没有增加有效的高优先级服务。修复因此针对 wakeup 的优先级关系和 GIC
所有权边界，而不是继续调 quantum 常数。

### 测试盲区及改进

- 旧 runner 没有 hold/release marker，无法区分 Guest 完成和 Guest 已关机；
  现在 parser 强制检查 marker 顺序。
- 旧镜像可能遮蔽源码修改；现在构建步骤显式重打包 initramfs，并在结果中
  保存 SHA256。
- 短 TCG smoke 不能外推长期 P99；现在保留失败日志、progress watchdog、
  VM-exit snapshots，并对所有百分位使用“受 censored/TCG 波动影响”的表述。
- host `cargo test -p axvm --lib` 不是有效的裸机链接验证；以 AArch64
  release build 和真实 QEMU 运行作为消费者验证。

### 可复用教训

1. 先固定拓扑，再只改变一个机制变量。
2. 任何“IRQ 返回即调度”的设计都必须先证明完成/EOI 顺序和切换频率上界。
3. 实时系统验收必须同时记录平均、P99、P99.9、最大值、样本完整性和
   长时间 liveness；单个漂亮数字不构成机制证据。
