# vIRQ A/B 实验总结与结果

> 实验日期：2026-08-09  
> 当前状态：正式 A/B 实验已完成；四轮均形成 300/300 guest 闭环，可进行延迟比较。

## 1. 实验目标与固定条件

本实验比较两条 software vIRQ 注入路径：

- **A（旧路径）**：host injector 将事件写入 VM 级的无界 pending queue，执行
  `notify_all → IPI(task.cpu_id())`，再由 vCPU run loop drain 并调用 backend 的
  `inject_interrupt`。
- **B（目标路径）**：host injector 将事件写入 per-vCPU 有界 dispatcher queue，使用
  vCPU affinity 选择目标 CPU，经定向 notify 和 IPI 进入 vCPU run loop，再执行
  `drain → inject`。

因此，当前 A/B 实际比较的是“旧的无界 VM pending queue/全局唤醒路径”和“新的有界
per-vCPU dispatcher/定向唤醒路径”，不是“直接 backend 注入”和“队列化注入”的对比。

固定输入如下：

| 项目 | 配置 |
| --- | --- |
| 实验顺序 | `A1 → B1 → A2 → B2` |
| injector 周期/次数 | 10 ms / 300 次 |
| VM/vCPU | VM 2 / vCPU 0 |
| software vIRQ | vector 48 |
| injector host CPU | CPU 0 |
| workload | `scripts/test/zephyr-soft-virq` |
| 观测事件 | `enqueue`、`notify`、`ipi`、`running`、`drain`、`inject`；B 另有 `queue_overflow` |
| guest warm-up | VM running 后 2 s，A/B 共用，不计入样本 |
| final grace | 最后一次注入后等待 1 个 10 ms 周期，再 dump trace |

代码所在工作树：

- B：`openrace/realtime-virq-ab`（`/home/huhu/tgoskits`）
- A：`contest/openrace-2026`（`/home/huhu/tgoskits-2026`）

## 2. 已完成且有效的部分

### 2.1 公共实验组件

- host injector 参数已固定，便于 A/B 使用同一输入。
- 已增加 per-host-CPU bounded trace，能够记录注入链路各阶段。
- Zephyr workload 能报告 `received/expected`，统计脚本位于
  `scripts/test/virq_latency_stats.py`。

### 2.2 B 代码侧改动

- 实现 per-vCPU 有界中断队列和定向 vCPU wait queue。
- 实现 `enqueue → notify → IPI → drain → inject` 路径。
- vCPU affinity 使用 vCPU 配置的物理 CPU mask，不再依赖 task 创建瞬间的 `task.cpu_id()`。
- 修复 enqueue 后 vCPU 入睡造成的 pending 丢唤醒。
- 修复 vCPU task 创建与私有 wait queue 发布之间的启动竞态。
- 首轮启动时出现的 `ESR_EL2=0x8a000000` / PPI26 风暴已不再阻塞后续 B 启动。

### 2.3 静态验证

以下检查通过：

```text
cargo fmt --all -- --check
cargo clippy -p axvm --features 'host-test realtime-trace' --lib -- -D warnings
cargo test -p axvm --features 'host-test realtime-trace'
```

测试结果：177 个 axvm 单元测试、19 个架构边界测试、4 个错误契约测试通过。测试编译过程中的 6 个 dead-code warning 是仓库已有 warning。

## 3. 失败与局部成功

### 3.1 A1：旧队列链路有发送记录，但没有 guest vIRQ

证据日志：`/tmp/ab-A1.log`

```text
enqueue=300
notify=300
ipi=300
running=1
drain=0
inject=0
guest: SOFTWARE VIRQ FAIL received=0 expected=300
```

A1 中可以看到 vCPU 的 `running` 事件发生在 host CPU 3，而 injector 的 `enqueue`、`notify`、`ipi` 记录发生在 CPU 0。但 trace 的 `cpu` 字段表示记录事件时的当前 CPU；`ipi` 事件没有记录目标 CPU，不能据此证明 IPI 发错 CPU。A 的实际路径是无界 pending queue 加 `task.cpu_id()` 目标选择，而不是从 CPU 0 直接调用 backend 注入。

本轮真正能确认的是：300 次 enqueue/notify/IPI 都有发送端记录，但没有任何 `drain` 或 `inject`。因此 pending interrupt 没有形成可观测的 host-side 注入闭环；究竟是 IPI 未送达、未触发 guest exit，还是退出后未进入 drain，现有 trace 还不能区分。该轮不能产生 latency 样本，也不能作为有效的 A/B p99 对比基线。

统计脚本结果：`injected=300`、`matched=0`、`lost_irq=300`，`p99/p99.9/max` 均为 0（表示没有样本，不是延迟为 0）。

### 3.2 B 首轮：启动竞态导致 vCPU 永久等待

证据日志：`/tmp/ab-B1.log`

vCPU task 在 per-vCPU wait queue 发布之前先睡到了旧的全局队列；之后 startup 通知只唤醒了新队列，task 因此永久等待。同时出现 `ESR_EL2=0x8a000000` 和 PPI26 unhandled IRQ 风暴。本轮没有有效实验数据，不能纳入结果。

### 3.3 B 修复后的 passthrough smoke：队列运行但无法 drain/inject

证据日志：`/tmp/ab-B1-target.log`

```text
enqueue=64
notify=64
ipi=64
running=1
drain=0
inject=0
queue_overflow=236
guest: received=0 expected=300
```

该轮已经可以启动，但在 `interrupt_mode = "passthrough"` 下，发送端虽然记录了 64 次 IPI，却没有观察到 `drain` 或 `inject`。现有 trace 没有 IPI 接收端和 guest exit 原因，因此只能确认 vCPU 没有可靠回到 AxVM host-side drain/inject 路径，不能进一步断言 IPI 是未送达还是未造成 guest exit。队列累积到容量 64 后，后续 236 次 enqueue 报错并记录为 overflow。

### 3.4 B 临时 emulated smoke：dispatcher 局部跑通，guest 闭环仍失败

证据日志：`/tmp/ab-B1-emu.log`

本轮临时将配置改为 `interrupt_mode = "emulated"`，实验后已恢复为 `passthrough` 以保持 A/B 输入一致。

```text
enqueue=65
notify=65
ipi=65
running=31
drain=1
inject=1
queue_overflow=235
guest: SOFTWARE VIRQ FAIL received=0 expected=300
```

这证明 B 的 dispatcher 至少能够执行一次 `running → drain → inject`，但 emulated GIC 路径仍未形成 guest 可观察的 software vIRQ 闭环。`running=31` 只能说明 host run loop 进入了 31 次，现有事件没有记录每次对应的 guest exit 原因，不能把这 31 次全部解释为启动阶段 WFI 退出。统计脚本报告 `injected=300`、`inject_errors=235`、`matched=0`，因此仍不能做延迟比较。

## 4. 正式 A/B 最终结果

最后一次注入后增加一个完整采样周期的 grace，避免大量串口 trace 输出污染最后一个
guest ISR 样本。四轮均满足 `VIRQ_INJECT_COMPLETE ... errors=0` 和
`SOFTWARE VIRQ COMPLETE samples=300`。

| 轮次 | guest 收到 | 注入错误 | 丢失 | overflow | mean (ns) | p99 (ns) | p99.9/max (ns) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| A1 | 300/300 | 0 | 0 | 0 | 449,638 | 1,051,552 | 1,599,552 |
| B1 | 300/300 | 0 | 0 | 0 | 392,565 | 1,138,720 | 1,370,992 |
| A2 | 300/300 | 0 | 0 | 0 | 452,003 | 1,163,136 | 1,272,896 |
| B2 | 300/300 | 0 | 0 | 0 | 470,652 | 1,077,840 | 1,163,168 |
| 两轮平均 | — | — | — | — | **A 450,820 / B 431,608** | **A 1,107,344 / B 1,108,280** | **A 1,436,224 / B 1,267,080** |

两轮平均相对 A：B 的 mean 低 4.26%，p99 高 0.08%（基本持平），p99.9/max 低
11.78%。样本只有两轮，且单轮波动明显，因此不能把 mean 或 p99 宣称为稳定收益；
可以确认的是 B 已经可靠完成注入闭环，且本实验中没有丢中断或队列溢出。尾部指标方向
对 B 有利，但还需要更多重复轮次或长稳实验才能作为强性能结论。

## 5. 双流最小压力场景（2 ms 双注入器）结果

在单 vCPU guest 上同时监听 vector 48/49，两个 host injector 分别固定在
CPU 0/1，各发送 300 次、周期 2 ms；使用 emulated GIC 配置（passthrough 下
vCPU 连续运行不退出，无法 drain）。每个分支 3 轮，全部 600/600 闭环、0 丢失、
0 注入错误、0 溢出。

| 轮次 | 闭环 | mean (ns) | p99 (ns) | max (ns) | lost |
| --- | ---: | ---: | ---: | ---: | ---: |
| A3 | 600/600 | 334,864 | 834,080 | 1,378,384 | 0 |
| A4 | 600/600 | 273,217 | 634,960 | 1,228,816 | 0 |
| A5 | 600/600 | 363,952 | 765,904 | 1,274,400 | 0 |
| B3 | 600/600 | 322,066 | 926,688 | 1,709,824 | 0 |
| B4 | 600/600 | 300,482 | 927,616 | 1,404,704 | 0 |
| B5 | 600/600 | 229,452 | 452,288 | 853,216 | 0 |

三轮平均：A mean 324,011 / p99 744,981 / max 1,293,867；B mean 284,000 /
p99 768,864 / max 1,322,581。B 的 mean 低约 12.3%，但 p99/max 跨轮波动大且
平均略差。因此本场景只能说明：B 在并发双注入下可靠完成闭环、无溢出无丢失；
性能方向对 B 的 mean 有利，但还不能作为强结论。

### 5.1 本轮修掉的实验链路问题

1. 用 Zephyr SDK 1.0.1 重新构建 guest 后 ELF 入口从 `0x40001044` 变为
   `0x400010b4`，旧 VM 配置导致启动失败和 PPI26 风暴，已同步修正 A/B 配置。
2. passthrough GIC 下 vCPU 在 guest 内连续运行、没有 guest_exit，队列只进
   不出直到溢出；成功链路必须使用 `axvisor-qemu-aarch64-emulated.toml`。
3. 注入器完成时调用 `dump_realtime_trace()` 会一次性输出数万行串口日志，
   让 QEMU 停顿约 1 s，同 vector 的待处理中断恢复后背靠背注入并被 GIC 合并；
   已移除注入器内部的 trace dump（溢出仍可通过 `inject_errors` 观测）。
4. guest 的 10 s 等待窗在平台停顿下太紧，放宽到 30 s。
5. 统计脚本需要剥离 ANSI 转义，否则串口交错会让 CSV 首行漏读。

### 5.2 仍不能证明的部分

单 vCPU 下 A 的 `notify_all` 没有额外 vCPU 需要唤醒，因此本场景不能证明 B 的
targeted notify 端到端收益。要证明这一点，仍需解决双 vCPU guest 启动链路；
当前数据只支持“并发生产者下的队列稳定性”这一范围。

## 6. 证据与复现入口

- A1：`/tmp/ab-A1.log`
- B 首轮：`/tmp/ab-B1.log`
- B passthrough smoke：`/tmp/ab-B1-target.log`
- B emulated smoke：`/tmp/ab-B1-emu.log`
- 统计：`python3 scripts/test/virq_latency_stats.py <log>`
- workload：`scripts/test/zephyr-soft-virq`
- 最终 A1：`/tmp/openrace-ab-grace-A1.log`
- 最终 B1：`/tmp/openrace-ab-grace-B1.log`
- 最终 A2：`/tmp/openrace-ab-grace-A2.log`
- 最终 B2：`/tmp/openrace-ab-grace-B2.log`
- 双流 A3/A4/A5：`/tmp/openrace-dualstream-emulated-A3.log` /
  `/tmp/openrace-dualstream-emulated-A4.log` /
  `/tmp/openrace-dualstream-emulated-A5.log`
- 双流 B3/B4/B5：`/tmp/openrace-dualstream-emulated-B3.log` /
  `/tmp/openrace-dualstream-emulated-B4.log` /
  `/tmp/openrace-dualstream-emulated-B5.log`

## 7. 双 vCPU 启动修复后的 A/B 复测（2026-08-09）

### 7.1 根因与修复

NEXT.md 3.1 的双 vCPU 启动故障根因是 **HVC 返回地址被重复 +4**：
QEMU/ARM 对 HVC 的 preferred return address 本来就是 `hvc` 的下一条指令，
而 `handle_hvc64_exception()` 又调用 `advance_aarch64_exception_pc()`
再 +4。Zephyr SMP 的 PSCI CPU_ON 路径 `hvc`（0x40004554）返回后应执行
`ldr x9,[sp]`（0x40004558），被跳过后在 0x4000455c 的 `stp x0,x1,[x9]`
因 x9=0 触发 FAR=0 的 data abort，guest 卡死。

修复（B `f837d3ad0` / A `418fa8be3`）：

- HVC64 / SMC64 不再二次 +4（它们的 ELR 已指向下一条指令）。
- DataAbort / SysReg 保留 +4：这两类异常的 ELR 指向被模拟的故障指令本身，
  模拟后必须跳到下一条。
- 两个单测从断言 `TEST_PC + 4` 改为断言 `TEST_PC`，先在旧实现上验证失败，
  再在修复后验证通过（qemu-aarch64 user 模式跑 `cargo test -p arm_vcpu`）。

### 7.2 双 vCPU 启动验收（A/B 均通过）

配置：`tmp/vmconfigs/zephyr-soft-virq-smp2.toml`（双 vCPU、emulated GIC、
SMP Zephyr guest，入口 0x400010e4）。验收输出：

```text
PSCI_CPU_ON target=0x1
VM[2] VCpu[1] running...
Secondary CPU core 1 (MPID:0x1) is up
SOFTWARE VIRQ READY streams=2 vector_base=48 samples=300
```

不再出现 `ELR_ELn / FAR_ELn` panic。A/B 各 1 次通过。

### 7.3 修复后标准双流 vIRQ 复测

与第 5 节相同的 emulated GIC 双注入器场景，guest 使用仓库源码重新构建的
`zephyr.bin`（旧二进制等待窗只有 9999 ticks，且 guest tick 前进偏快，
会在样本尾部提前超时）。A/B 各 3 轮，全部 600/600 闭环、0 丢失、
0 注入错误、0 溢出。

| 轮次 | 闭环 | mean (ns) | p99 (ns) | max (ns) | lost |
| --- | ---: | ---: | ---: | ---: | ---: |
| A1 | 600/600 | 362,146 | 727,760 | 902,464 | 0 |
| A2 | 600/600 | 297,280 | 748,048 | 1,000,304 | 0 |
| A3 | 600/600 | 274,591 | 766,736 | 1,025,280 | 0 |
| B1 | 600/600 | 275,498 | 499,888 | 644,368 | 0 |
| B2 | 600/600 | 320,912 | 1,013,840 | 1,524,544 | 0 |
| B3 | 600/600 | 307,376 | 733,200 | 1,283,840 | 0 |

三轮平均：A mean 311,339 / p99 747,515 / max 976,016；B mean 301,262 /
p99 748,976 / max 1,151,917。B 的 mean 低约 3.2%，p99 基本持平，max 仍受
单轮长尾波动影响，与修复前结论方向一致；本轮主要价值是确认 HVC 修复没有
改变单 vCPU 双流注入的闭环质量。

### 7.4 新发现的限制：双 vCPU 上的软件 vIRQ 注入

双 vCPU guest 完整启动后，标准注入器（固定在 host CPU 0/1）在
`phys_cpu_ids=[0,1]` 配置下 **0 个 vIRQ 到达 guest**：

- guest 的 `k_sleep` 只执行 `wfi`（HCR_EL2 未设 TWI），guest 睡眠期间没有
  VM exit；vCPU task 在 host CPU 0/1 上连续占用 CPU 且 host IRQ 屏蔽，
  同核上被 pin 的 injector task 无法被调度（探针显示 injector 连
  `before-inject` 都打不出）。
- 把 vCPU 移到 `phys_cpu_ids=[2,3]` 后 injector 恢复运行，但 guest MPIDR
  与 `phys_cpu_ids` 绑定（`mpidr_el1 = placement.phys_cpu_id`），主核变成
  MPIDR=2，SMP guest 不再走 primary boot 路径，guest 完全不输出。

结论：双 vCPU 的“跨核定向唤醒 + 注入”实验还需要（任选其一）HCR_EL2.TWI
trap WFI、把 injector 与 vCPU 的 host CPU 分离且保持 MPIDR 0/1、或为
MPIDR 与 host affinity 提供独立配置。当前数据不覆盖该场景。

### 7.5 本轮日志

- A 双核启动：`/tmp/ab-A-dual-boot.log`
- B 双核启动：`/tmp/ab-B-dual-boot.log`
- 双流 A1/A2/A3：`/tmp/ab-A-standard-1.log` / `-2.log` / `-3.log`
- 双流 B1/B2/B3：`/tmp/ab-B-standard-1.log` / `-2.log` / `-3.log`
