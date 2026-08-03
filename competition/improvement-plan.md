# Competition Improvement Plan

> 状态：执行中（M0/M1 调试闭环和 M2 无损采集已通过；PPI27 热路径串口扰动已从根因消除，修复后的 AxVisor host-noise 实体 smoke 已按 AB/BA/AB 完成 3/3 组，direct IRQ p99/max 三组均改善约 91%/83%–86%；这些运行来自 dirty worktree 且每项仅 20 个用户态样本，因此 clean-commit 正式矩阵仍为 0/5，最坏延迟正式出口门尚未通过）
>
> 更新日期：2026-08-03
>
> 范围：补齐 `competition/requirement.md` 中 StarryOS 实体板、实时最坏延迟、手动控制基线、协议故障和重复性证据；以 ONNX 为唯一跨后端部署模型来源，在 StarryOS 上形成 RKNN NPU 主后端及 ONNX Runtime CPU 对照后端。
>
> 非目标：本计划不引入模型训练，也不提前创建比赛源码 PR。

## 1. 目标与优先级

| 优先级 | 当前不足 | 本计划的完成目标 |
| --- | --- | --- |
| P0 | 任务一仍由 Linux/QEMU 完成 | 在 OrangePi 5 Plus 上由 StarryOS 完成任务一，并保留 shared/partitioned 同源对照 |
| P0 | 实时测试没有证明最坏延迟改善 | 增加直接虚拟定时器/IRQ 延迟指标，以多次配对运行证明 p99 和 worst-case 同时改善 |
| P0 | 实体板没有 StarryOS 手动控制基线 | 在同一实体板、同一工作负载下完成 StarryOS manual/neural 配对实验 |
| P1 | ERROR、重启恢复、ACK 丢失缺少实体跨客户机证据 | 为三种故障建立可自动结束、可重复的实体板 profile |
| P1 | 实体板 full/smoke 只有一次运行 | full 至少 5 次，smoke 和故障 profile 至少 3 次，并统计跨运行最坏值 |
| P2 | 4×6×1 网络缺少标准模型来源和硬件推理证据 | 不训练；把固定权重确定性导出为 ONNX，同源生成 `.rknn` 与 `.ort`，优先在 StarryOS 实体板完成 RK3588 NPU 推理，并保留 ONNX Runtime CPU 对照 |

完成顺序为：

1. M0：统一自动化和证据格式。
2. M1：补齐 StarryOS 实体手动基线。
3. M2：让 StarryOS 完成任务一并证明最坏延迟改善。
4. M3：补齐故障和重复性证据。
5. M4：建立同源 ONNX、RKNN NPU 主后端和 ONNX Runtime CPU 对照后端。
6. M5：汇总报告、复现材料和视频。

M4 的 WSL2 转换、宿主差分和构建 spike 可以在等待实体板时进行，但不能抢占或阻塞 P0/P1 工作。M4-core 的 RKNN NPU 闭环是正式交付项；M4-plus 的 ONNX Runtime CPU 实体闭环通过可行性门后再进入正式矩阵。

## 1.1 当前执行快照

已完成的 M0 调试闭环：

- WSL2 单命令完成 AxVisor 构建、OrangePi 启动、StarryOS/Zephyr 双客户机运行、结果盘快照、Linux 恢复和证据采集。
- StarryOS neural/manual smoke 各累计 3 次；每次均完成 20/20 命令，零错误、零超时、零重传、零恢复和零 deadline miss。
- 6 次 smoke 都来自同一实体板 `bf61f4d4a1d994ad`。manual 的 RMSE/IAE 固定为 `33170.156`/`64133.3`，neural 固定为 `31799.089`/`61355.8`，调试数据中分别改善约 4.1%/4.3%；正式结论仍等待 full 配对运行。
- 两组 dirty-worktree full AB/BA 调试配对已取得完整 raw 数据：每种策略每轮 1800 个样本且零 deadline miss。两组中 neural 的 RMSE/IAE 相对 manual 都改善约 35.9%/51.9%，但最大超调都由 `6840` 增至 `13428`，该退化必须进入最终报告；延迟差异处于几十微秒量级且方向不稳定。
- 64 MiB volatile virtio block backing 已持久化，Linux 侧 `e2fsck -fn`、镜像大小和 SHA-256 校验通过。
- 客户机 raw CSV 的 20 个样本已从快照中提取；控制器打印的 raw SHA-256 与 Linux 采集结果一致。
- `metadata.json`、`summary.json`、`raw.csv.gz`、`console.log.gz` 和 `checksums.sha256` 已生成并在 WSL/Windows 两侧通过哈希复核。
- 实体分析器以 raw CSV 重算延迟、控制质量、吞吐量和 deadline miss；提供 raw CSV 时，允许 UART 指标摘要缺失，但仍交叉校验任何完整摘要。
- UART 中粘连的多条 guest-console 记录会按前缀重新切分；结果/可靠性、RTOS poweroff、快照、Linux 恢复和哈希等验收项仍不得缺失。
- 当紧凑 outcome/reliability 副本全部截断时，分析器允许完整的冗余 `IVC-CONTROLLER-RESULT` 兜底；两种记录都完整却冲突时仍拒绝证据。
- 共享 CH340 UART 的启动标记采用错峰重复发送；控制器在 RTOS 收尾后等待 250 ms 再发送两组紧凑摘要，最新实体 smoke 的 12 条紧凑记录全部完整；自动快照命令限制为包含换行不超过 32 字节，避免 RX FIFO 丢字。
- 超时和未确认同步路径会拒绝断电；已验证通过短命令 `sync-host` 取得三次同步标记后才允许恢复 Linux。
- StarryOS 实体 RT compatibility smoke 已在 AxVisor 双 vCPU guest 上通过：绝对 `clock_nanosleep`/affinity、`SCHED_FIFO`、pthread/eventfd、timerfd 各完成 100 次，CPU stress 的 READY/STOPPED 生命周期完整，且板卡随后自动恢复 Linux。该结果说明任务一探针当前不需要先修改 StarryOS syscall；逐样本 UART 仍有截断，因此它只作为兼容性门，正式 RT raw samples 必须写入 guest 文件并通过块设备快照回收。
- StarryOS RT 无损采集已打通：客户机先在内存/临时文件中完成采样，再把每项 100 条样本合并写入 `/var/lib/axvisor-rt/raw.log`；shared 与 partitioned 快照均通过 `e2fsck -fn`，每次严格回收 300/300 条样本。shared 的快照内 raw SHA 与 UART 公布值完全一致；partitioned 即使 UART SHA 行被截断，快照 raw 仍可独立验证。
- 首组 dirty-worktree idle smoke 配对已经完成，同一 kernel、DTB、rootfs、探针、vCPU 映射和采样参数下只切换 `dedicated_cpus`。periodic jitter p99 改善 1.063%，但 max 退化 31.907%；dispatch p99/max 分别退化 2.140%/55.924%。因此当前数据不支持“partitioning 已改善最坏延迟”，M2 出口门仍为失败状态。
- `analyze_starry_board.py` 会拒绝缺样、重复完成标记、错误 CPU、错误 workload 和不完整生命周期；`compare_starry_board.py` 会拒绝非正交配对并按“正数表示 partitioned 更低”输出 p99/max 差值。AxVisor RT Python 回归当前 49/49 通过。
- 采集 runner 不再删除 live-ext4 中的三个 per-metric 临时文件，避免快照出现 orphan-list 记录。此前不干净的诊断快照只在“原镜像直接提取”和“副本修复后提取”逐字节一致时接受，且不纳入本次配对。
- 直接 IRQ 测量链路已在实体板打通：AxVisor 和 StarryOS 分别使用预分配固定环记录虚拟定时器注入与 guest IRQ handler 入口，统一使用 24 MHz guest virtual counter 域；host trace 同时导出 pCPU running/idle 和 vCPU runtime/映射。首组 shared/partitioned direct smoke 各取得 374 条 host 注入记录和 249 条完整配对，均为零丢样、零未完成记录、零注入失败、零计数器频率不一致；两个 vCPU 分别固定在 pCPU 1/2，迁移数为零。
- direct smoke 的 `virtual_timer_injection_to_guest_irq_ns` 在 shared 下 p99/max 为 2,275,000/4,947,833 ns，在 partitioned 下为 2,288,416/4,960,958 ns，分别退化 0.590%/0.265%。这说明测量与负载证据已经补齐，但 idle 场景下仅启用 `dedicated_cpus` 尚未改善直接 IRQ 长尾，当前仍不得宣称 M2 达标。
- 第二组 `cpu-stress` direct smoke 配对也已完成。direct p99/max 在 shared 下为 2,196,833/4,948,125 ns，在 partitioned 下为 2,200,916/4,949,000 ns，分别退化 0.186%/0.018%；periodic jitter 退化 1.752%，dispatch 改善 0.695%，timerfd proxy 退化 18.257%，因此 `m2_exit_gate_met=false`。
- `cpu-stress` 只让同一 StarryOS VM 的 guest CPU1 忙循环；shared 与 partitioned 的 host pCPU1 busy 分别为 99.750%/99.796%，pCPU2 busy 分别为 83.189%/83.229%，两个 vCPU 均零迁移。它证明 guest stress 生命周期和负载采集有效，但没有制造“只有 shared 侧存在”的跨 VM 或宿主竞争，不能作为 `dedicated_cpus` 隔离效果证据。
- 为构造跨 VM 干扰，已增加同一 180 秒、24 MHz virtual-counter 的 AArch64 noise guest，并把实验性 profile 与常规单 guest profile 隔离。初版多 pCPU affinity 在迁移后触发 hwirq 26/current-EL data-abort 风暴；改成共享侧单核 pCPU1 后，两条 vCPU 虽均按预期固定在 pCPU1，noise 首次运行约 9 ms 后仍触发 `ESR_EL2=0x96000021` 并破坏宿主串口输出。`NoPreempt` guest run-slice 已消除原来的 nested-vCPU panic，但没有证明同一 pCPU 上两个 AArch64 vCPU 能安全轮转。该路径当前是 M2 的明确 blocker，不得采集 partitioned 数值拼成无效对比。
- 噪声 profile 隔离后，常规单 guest shared 实体回归再次通过：StarryOS 双 vCPU 固定在 pCPU1/2，CPU-stress 生命周期完整，三项各完成 20 个样本，64 MiB snapshot、695 条 host trace、filesystem sync 和 `/dev/mmcblk1p2` Linux 恢复全部成功，且无 `ESR_EL2`/panic。它证明主采集路径未被实验性第二 VM 破坏，但仍只是 dirty-worktree 健康检查。
- 已实现维护用的 AxVisor host-noise 路线：顶层 `[host_noise]` 配置在默认 VM 启动前创建有界 busy-loop，使用 round-robin 调度，shared 固定 pCPU1、partitioned 固定 pCPU3，最长 180 秒；guest 结束后停止并把请求/观测 affinity、起止 tick、迭代数、停止原因和逐 pCPU wall ticks 同时写入 UART 与持久 host trace。分析器会拒绝缺失、越界、错误 placement、`max-duration` 或未覆盖完整 host trace 的记录。
- 初版 host-noise smoke 的 shared UART 记录了 `5,825` 次 pCPU1 `Unhandled IRQ ... hwirq 27`，partitioned 为零；该组 direct IRQ p99/max 改善 `91.498%/88.171%`，但同步串口 warning 会放大长尾，因此只保留为根因诊断，不能进入改善结论。
- 根因是每次 VM exit 保存了 guest `CNTV_CTL_EL0`/`CNTP_CTL_EL0`，却让定时器源在宿主任务运行期间继续使能。修复流程现在先保存状态并完成 GIC 应答/硬件 LR 转移，再关闭本地 guest 定时器，且在下一次 guest entry 恢复 `CVAL/CTL`。实体板回归证明不能在 GIC 应答前关闭 level PPI，否则客户机会卡在中断初始化。
- 修复后的 host-noise smoke 使用同一 StarryOS kernel、DTB、64 MiB idle rootfs、双 vCPU 映射、1 ms 周期和每项 20 个用户态样本，按 AB/BA/AB 完成三组。pair-1 的 shared/partitioned direct IRQ p99/max 为 `41,902,000/44,061,208 ns` 与 `3,745,291/7,597,916 ns`，改善 `91.062%/82.756%`；pair-2 为 `43,193,500/49,487,083 ns` 与 `3,751,708/7,806,458 ns`，改善 `91.314%/84.225%`；pair-3 为 `41,842,791/53,339,125 ns` 与 `3,728,083/7,655,958 ns`，改善 `91.090%/85.647%`。
- 三组共六次运行的 `unowned_virtual_timer_irqs`、`dropped`、`incomplete`、`failed_injections` 和 `counter_frequency_mismatches` 均为零，所有控制台的未处理 IRQ、`ESR_EL2`、panic 和 nested-vCPU 标记也均为零；placement、coverage、snapshot/fsck、同步后恢复 Linux 及零迁移全部通过。重复 smoke 门已达到 3/3，但每个 comparison 仍正确输出 `m2_exit_gate_met=false`，clean-commit 正式矩阵仍为 0/5。
- 板卡 gate 现在只把 `AXVISOR_SNAPSHOT_SYNC_OK` 作为终止成功条件，host-noise 完成由回收分析器独立验证；本地实体运行必须经仓库内 `competition/ivc/orangepi/board-runner.sh`，由它完成 SSH 重启、临时 `uboot-shell` 兼容补丁、串口 lease、同步后冷启动和 Linux 根文件系统恢复，不得用裸 `cargo xtask axvisor board` 代替完整状态机。
- Windows 与 WSL2 的本次 host-noise/PPI27 修复源码、配置、分析器、说明和证据已精确同步；AArch64 timer/GIC 顺序回归、RT trace 合约、Python 分析器、shell runner、格式化、相关 crate clippy，以及 shared/partitioned 两套 OrangePi 构建均已通过。

当前调试证据位于
`competition/results/orangepi-5-plus/snapshot-shortcmd-20260803/smoke/run-001/`。
同板手动基线调试证据位于
`competition/results/orangepi-5-plus/snapshot-manual-rawauth-20260803/manual-smoke/run-001/`。
另外两组重复运行位于 `snapshot-neural-repeat-20260803/` 和
`snapshot-manual-repeat-20260803/`。这些运行都来自 dirty 工作树，只证明自动化闭环，不计入正式比赛统计；UART 摘要与 poweroff marker 不完整的失败运行继续保留在 WSL 结果目录中。
首组 full 调试配对位于 `debug-full-pair-ab1-20260803/`，同样不计入正式统计。
第二组 BA 调试配对位于 `debug-full-pair-ba2-20260803/`；其中 manual 的原 runner 失败状态和修复后生成的 `summary-replay.json` 同时保留，未改写原始 metadata。
首组 StarryOS RT 无损 shared/partitioned 调试配对位于
`competition/results/orangepi-5-plus/starry-rt-debug-pair-20260803/`；目录同时保留失败尝试、raw、console、summary、comparison、metadata 和完整 checksum。该配对来自 dirty 工作树，只用于验证 M2 采集链路并暴露当前 worst-case 退化，不计入正式统计。
直接 IRQ trace 的首组 shared/partitioned 调试配对位于
`competition/results/orangepi-5-plus/starry-rt-direct-smoke-20260803/`；其中包含 rootfs raw、压缩 guest IRQ trace、host trace、逐配置 summary、正交 comparison 和失败的长串口命令尝试。该配对同样来自 dirty 工作树，只验证直接测量链路，不计入正式统计。
guest CPU-stress direct 调试配对位于
`competition/results/orangepi-5-plus/starry-rt-direct-stress-smoke-20260803/`；其中包含两侧 raw、guest/host trace、summary 和 comparison。该配对证明负载存在，但同时证明该负载不产生隔离变量。
跨 VM noise 的失败诊断位于
`competition/results/orangepi-5-plus/starry-rt-cross-vm-noise-smoke-20260803/`；目录保留 counter-frequency、nested-vCPU、run-slice、迁移和 singleton 同核轮转等每次失败日志。它只用于根因定位，不能进入延迟统计。
拆分 profile 后的单 guest 健康检查日志位于
`competition/results/orangepi-5-plus/starry-rt-single-guest-regression-20260803/`；该日志只验证常规主路径仍可完成，不纳入改善统计。
首组 AxVisor host-noise 受控干扰实体 smoke 位于
`competition/results/orangepi-5-plus/starry-rt-host-noise-smoke-20260803/`；目录包含两侧 console、raw、guest IRQ trace、host trace、summary 和 comparison。该配对来自 dirty 工作树，只证明路线可行并暴露 hwirq 27 日志扰动，不纳入正式统计。
PPI27 修复后的三组有效 AB/BA/AB smoke 位于
`competition/results/orangepi-5-plus/starry-rt-host-noise-ppi27-fix-smoke-20260803/`；目录包含原始 console、raw、guest/host trace、summary、comparison、冻结配置和校验和。这三组证明移除 UART 热路径扰动后改善方向仍一致，但仍来自 dirty 工作树且每项只有 20 个用户态样本，不纳入正式统计。

下一步按顺序执行：

1. 保持常规 `starry-rt-shared/partitioned` 为单 guest 可用基线；跨 VM noise 只允许通过显式 `starry-noise-*` 诊断 profile 启动，不再作为当前正式路线。
2. 冻结已通过实体回归的 AxVisor host-noise/PPI27 修复：pCPU1/pCPU3、RR、180 秒上限、先 GIC 应答后关闭 guest timer、持久 trace schema、`unowned_virtual_timer_irqs=0` 和 snapshot-sync 唯一成功门均不得随实验轮次变化。
3. 从当前变更生成可审计的 clean commit，冻结正式矩阵的 commit、镜像/配置哈希、五组 AB/BA 顺序、样本数、门槛和结果目录；调试 smoke 不得重新标记为正式证据。
4. 在该 clean commit 上执行 5 组 AB/BA、每侧每次至少 10,000 个主要指标样本，再执行 shared/partitioned 各 30 分钟 soak；每半次运行后立即回收 `/home/rt`，保留所有失败运行，并按 pair 报告 p99/max、4/5 方向门和 worst-of-runs。
5. 完成 M2 正式门后再按顺序补齐 M3 的 ACK loss/ERROR/restart 实体证据；M4 的 ONNX/RKNN/ORT 无板 spike 可利用板卡空闲并行准备，但不得抢占 P0/P1 板卡矩阵。

## 2. 执行约束

### 2.1 WSL2 自动化主机

WSL2 是唯一板卡自动化主机，负责构建、部署、串口控制、结果采集和分析。正式结果不得依赖未纳入仓库的 `~/.local/bin` 脚本。

实体板运行固定采用以下状态机：

1. 通过本地 board service 获取 OrangePi 5 Plus lease。
2. 启动板卡 Linux，发现 IP，并通过 SSH/rsync 部署文件。
3. 在 Linux 中校验目标文件、权限和哈希。
4. 显式执行 `sync`，记录 `AXVISOR_HOST_FILESYSTEM_SYNCED`。
5. 退出并释放 Linux `board connect` lease。
6. 重新获取 lease，启动 AxVisor/StarryOS 实验。
7. 串口只采集状态和摘要；高频原始样本写入客户机文件系统。
8. 实验结束后再次显式同步并恢复 Linux。
9. 通过 SSH 收回原始样本、日志和元数据，最后释放 lease。

当前 OrangePi/AxVisor 实体运行统一使用仓库内 wrapper；它会注入 host-noise
配置，使用唯一 snapshot-sync 成功门，并在结束后恢复 Linux：

```bash
ORANGEPI_AXVISOR_BUILD_CONFIG=scripts/benchmark/axvisor-rt/config/axvisor-orangepi-5-plus-starry-host-noise-shared.toml \
ORANGEPI_AXVISOR_BOARD_CONFIG=scripts/benchmark/axvisor-rt/config/board-orangepi-5-plus-starry-host-noise-shared.toml \
ORANGEPI_AXVISOR_SHUTDOWN_MARKER_REQUIRED=1 \
ORANGEPI_RESTORE_LINUX=1 \
bash competition/ivc/orangepi/board-runner.sh
```

partitioned 半轮只替换两项配置为对应的 `partitioned.toml`。不得以裸
`cargo xtask axvisor board` 替代 wrapper：本机 board service 的 power 命令为空操作，
而 wrapper 还负责 Linux SSH 重启、U-Boot 2025.10 兼容配置、精确 TF 卡启动命令和
`/dev/mmcblk1p2` 可写 ext4 验证。

文件系统同步失败时停止实验，不通过冷重启绕过失败。只有在确认没有文件写入和板卡测试运行时，才允许使用电源控制恢复失联板卡。

### 2.2 实现和验证规则

- 修改 Rust 代码前完整阅读 `book/guideline/code-quality.md`。
- 若测试暴露 StarryOS syscall/Linux ABI 语义问题，再完整阅读 `book/guideline/starry/syscall.md` 后修复；不为加分项添加无实际需要的 syscall。
- 修复缺陷时先增加必然失败的确定性回归测试，确认红灯后再实现修复。
- ArceOS、StarryOS 和 AxVisor 的构建/测试优先使用 `cargo xtask`。
- Rust 代码修改后运行 `cargo fmt` 和受影响 crate 的目标化 `cargo xtask clippy --package <crate>`。
- 正式板卡结果必须来自干净 commit；工作树 dirty 的运行只能作为调试结果。
- 若修改物理板启动、SMP、guest DTB 或推荐调试流程，同一变更更新 `.claude/skills/arch-platform-porting/` 的对应说明。
- AArch64 guest DTB 只描述 guest 可见的 GICv3、architectural timer、PL011 和 virtio-mmio，不暴露完整 RK3588 host tree。
- GICR frame 保持 128 KiB、不重叠，仅最后一个 redistributor 设置 `GICR_TYPER.Last`；guest CPU ID 与 host scheduler ID 分离。

## 3. M0：统一自动化和证据格式

### 3.1 仓库内板卡 runner

改造 `competition/ivc/run-orangepi-5-plus.sh`，或在仓库内增加其直接调用的 runner，消除对用户目录外部实现的依赖。

runner 至少支持：

```text
--profile <name>
--repeat <count>
--board OrangePi-5-Plus
--result-dir <path>
--timeout <seconds>
--restore-linux
```

必须处理：

- lease 获取、释放和异常清理；
- Linux 部署、哈希校验和显式同步；
- 重启脚本调用；
- 串口成功/失败 marker；
- 超时和板卡失联；
- Linux 恢复及结果回收；
- 中断后不会把 lease 永久留在占用状态。

### 3.2 profile 参数化

将 `competition/ivc/starry/build-rootfs.sh` 的 profile 拆成正交参数：

```text
policy  = manual | neural
backend = native | onnxruntime-cpu | rknn-npu
profile = smoke | full | fault-ack-loss | fault-error | fault-restart
period  = <milliseconds>
count   = <samples>
repeat  = <run-id>
```

避免为每一种组合复制一份 rootfs 脚本。

### 3.3 统一结果目录

每次运行生成：

```text
<result-dir>/<profile>/<run-id>/
├── metadata.json
├── summary.json
├── raw.csv.gz
├── console.log.gz
└── checksums.sha256
```

`metadata.json` 至少包含：

- Git commit、branch 和 dirty 状态；
- AxVisor、StarryOS kernel、DTB、rootfs 和客户机镜像 SHA-256；
- profile、run ID、重复次数和执行顺序；
- board type、board ID、CPU 温度和 UTC 时间；
- 模型 ID、ONNX 来源 SHA-256、实际部署模型 SHA-256、推理后端和精度；
- ONNX Runtime、RKNN Toolkit/Runtime、RKNPU 驱动版本及 NPU core mask（按实际后端填写）；
- 样本数、丢样数、退出状态和成功 marker。

分析器必须从 `raw.csv.gz` 重新计算指标，而不是信任客户机打印出的 summary。

### 3.4 M0 退出条件

- [x] 完整 smoke 可以由 WSL2 单命令完成；当前为 dirty-worktree 调试证据。
- [x] Linux 部署、StarryOS raw marker 和回收文件的哈希一致。
- [x] 中断、超时和失败路径会释放 lease，并只在确认文件系统安全后恢复 Linux。
- [x] metadata、summary、raw data 和 checksum 已由 57 个 competition 回归测试及实体结果自检校验。
- [ ] runner 的 U-Boot 2025.10 兼容仍临时依赖本机 `uboot-shell` patch；上游修复 [drivercraft/ostool#164](https://github.com/drivercraft/ostool/pull/164) 尚未合并，待发布 0.2.7 后移除本地 Cargo patch。
- [ ] 在 clean commit 上重新运行并保留正式 M0 smoke 结果。

## 4. M1：StarryOS 实体手动控制基线

### 4.1 实现

`competition/ivc/starry/autorun.sh` 已支持 `ivc_mode=manual`，因此优先复用现有控制器，只增加以下正式 profile：

- `starry-manual-full`
- `starry-neural-native-full`
- `starry-manual-smoke`
- `starry-neural-native-smoke`

manual 和 neural 必须使用相同的：

- OrangePi 5 Plus；
- AxVisor、StarryOS 和 Zephyr 二进制；
- vCPU/IRQ 配置；
- 初始状态、输入序列、周期和样本数；
- 串口与网络拓扑。

两者只有 `policy` 不同。开始配对测试前冻结 native 模型权重和模型哈希，正式结果产生后不依据结果重新调权。

### 4.2 实验设计

- full：manual/neural 配对运行 5 组。
- 运行顺序采用 AB/BA 交错，降低温度和运行时长漂移影响。
- 每次 full 保留逐样本控制输入、模型输出、执行器命令、状态和端到端延迟。
- smoke：每个最终镜像至少运行 3 次，用于确认镜像健康，不作为主要统计结论。

分析指标：

- RMSE；
- IAE；
- 超调量；
- 稳定时间；
- 端到端延迟 p50/p95/p99/max；
- 丢包、重发和控制周期 miss。

### 4.3 M1 退出条件

- [ ] 五组 manual/neural 配对实验全部成功。
- [ ] 每组原始样本数、哈希和执行顺序完整。
- [ ] 至少两项控制质量指标在 neural 中稳定优于 manual。
- [ ] 所有退化项也进入报告，不选择性隐藏结果。
- [ ] `competition/test-report.md` 的手动基线改为 StarryOS 同实体平台数据。

## 5. M2：StarryOS 实体任务一和最坏延迟证明

### 5.1 StarryOS RT 探针兼容

以 `scripts/benchmark/axvisor-rt/guest/axvisor_rt_probe.c` 为起点，制作 StarryOS RT rootfs，并按以下顺序验证：

1. `clock_nanosleep(TIMER_ABSTIME)` 周期唤醒；
2. CPU affinity；
3. `SCHED_FIFO`；
4. pthread/eventfd dispatch；
5. timerfd；
6. CPU stress 和长时间运行。

先运行每项 100 次的 compatibility smoke。只有确认 StarryOS 的实现缺少所需语义时才修改内核，并先增加回归测试。

当前状态：上述 compatibility smoke 已在 OrangePi 5 Plus 的 AxVisor/StarryOS
双 vCPU guest 上通过，未发现需要修改内核的 syscall 语义缺口。兼容性日志保留
UART 截断事实，不作为正式延迟样本。探针 stdout 已写入 guest rootfs，并通过
volatile virtio-blk snapshot 流程回收无损 raw data；每项 100 条样本的 shared 和
partitioned idle smoke 均完成 300/300 条严格校验。

### 5.2 改造前后同源配置

建立两套由同一 commit 构建的 profile：

- `starry-rt-shared`：固定两个 timer-owning vCPU 的初始/允许 pCPU 集，但不把这些
  pCPU 从其他共享 guest vCPU 的候选集合中排除，作为改造前基线；
- `starry-rt-partitioned`：保持相同 vCPU/pCPU 映射，并从其他共享 guest vCPU 的
  有效 affinity 中排除所保留的 pCPU，作为改造后结果。

shared 不允许 vCPU 在全部 pCPU 间迁移：RK3588 上 hardware-backed virtual timer
PPI 由当前物理 CPU 持有，睡眠中的 vCPU 迁移会丢失唤醒。两套 profile 因此只用
singleton affinity 固定 timer-owning vCPU，避免把 timer 正确性缺陷混入分区性能
对照。

`dedicated_cpus` 的实际边界必须在报告中写清：它约束的是 AxVM 注册的 guest vCPU
task，不会自动把普通 AxVisor host task、housekeeping 或物理 IRQ 从保留核迁走。
同一 VM 内 guest CPU1 的 busy loop 也只增加其 vCPU task 的运行时间，shared 和
partitioned 都会把该 vCPU 固定在 pCPU2，因此不能制造隔离差异。

正式干扰实验必须在两侧运行相同实现、时长和强度的独立干扰源，并显式改变其
placement：shared 侧与 StarryOS vCPU0 同置 pCPU1，partitioned 侧放到 pCPU3。
这属于“共享放置与隔离放置”的处理变量，不得写成只切换 `dedicated_cpus` 的纯
布尔对比。当前第二 guest 路线受 AArch64 vCPU 轮转故障阻塞；在修复前优先使用
可控的 AxVisor host noise task，并独立记录其 affinity、运行时长和实际 pCPU 时间。

两套配置的 kernel、rootfs、探针、采样周期、样本数和压力负载保持一致。不得把不同 commit、不同客户机或不同硬件的结果直接作为前后对照。

### 5.3 直接最坏延迟指标

timerfd 保留为用户态 proxy，但不能作为唯一 IRQ 结论。增加以下直接测量：

1. AxVisor 在虚拟定时器到期/注入时记录 ARM counter 和序号。
2. StarryOS 在对应 IRQ handler 入口记录 ARM counter 和序号。
3. 先验证 host/guest counter offset 和换算频率。
4. 在离线分析中计算 `virtual_timer_injection_to_guest_irq_ns`。
5. IRQ 热路径使用预分配、无阻塞的记录缓冲，不进行串口打印或动态分配。

同时增加 AxVisor 侧：

- 每个 pCPU 的运行时间和 idle 时间；
- 每个 vCPU 的运行/等待时间；
- IRQ 计数、定向 CPU 和最大 handler 时间；
- 样本溢出和丢失计数。

不得再用 guest `/proc/stat` 代替 AxVisor host pCPU 负载。

当前实现状态：host/guest 固定环、counter-domain 校验、离线配对、结果盘回收和
pCPU/vCPU accounting 均已通过 OrangePi 5 Plus 实体 smoke。IRQ 入口会先结束当前
pCPU 的 architectural-idle 区间，避免把 IRQ handler 执行时间误计为 idle；trace
热路径不分配内存、不加锁、不打印，导出在 guest 结束后进行。

### 5.4 实验矩阵

| 配置 | 负载 | 重复次数 | 每次样本 | 额外要求 |
| --- | --- | ---: | ---: | --- |
| shared | idle | 5 | >= 10,000 | 与 partitioned 配对 |
| partitioned | idle | 5 | >= 10,000 | 与 shared 配对 |
| shared | guest CPU1 stress | 5 | >= 10,000 | 兼容性/同 VM 负载，不作为隔离证明 |
| partitioned | guest CPU1 stress | 5 | >= 10,000 | 与 shared 配对，确认负载路径稳定 |
| shared | controlled interference | 5 | >= 10,000 | 干扰源与 vCPU0 同置 pCPU1 |
| partitioned | controlled interference | 5 | >= 10,000 | 同一干扰源固定 pCPU3 |
| shared | interference soak | 1 | >= 30 分钟 | 报告全程 worst-case |
| partitioned | interference soak | 1 | >= 30 分钟 | 报告全程 worst-case |

主要指标预先固定为：

- `virtual_timer_injection_to_guest_irq_ns`；
- periodic wake-up jitter；
- dispatch latency。

timerfd proxy 和吞吐量为次要指标。

#### 5.4.1 当前调试配对

首组 idle smoke 正交配对已完成，但不满足正式统计规模：

| 指标 | shared p99/max | partitioned p99/max | p99 改善 | max 改善 |
| --- | ---: | ---: | ---: | ---: |
| periodic jitter | 31,417 / 32,125 ns | 31,083 / 42,375 ns | +1.063% | -31.907% |
| dispatch latency | 40,834 / 61,542 ns | 41,708 / 95,959 ns | -2.140% | -55.924% |
| timerfd proxy | 46,792 / 46,917 ns | 46,833 / 47,042 ns | -0.088% | -0.266% |

两项主要 guest 指标的 p99 均未超过 5% 退化门槛，但两个 max 都退化，因此
`m2_exit_gate_met=false`。该结果优先推动以下定位，而不是扩写“已优化”的结论：

1. 增加 direct IRQ trace，区分 timer 注入、guest IRQ entry 与用户态唤醒的长尾；
2. 增加 host pCPU/vCPU accounting，确认 timer-owning vCPU placement 和实际负载；
3. 运行固定 guest CPU stress 的小样本配对，判断 idle 单次 outlier 是否可重复，并
   验证该负载是否真正产生配置间差异；
4. 上述证据完整后再扩展到 10,000 样本和 5 组配对。

#### 5.4.2 当前直接 IRQ trace 调试配对

在同一 trace-enabled StarryOS kernel、DTB、64 MiB rootfs、双 vCPU 映射和 idle
采样参数下，仅切换 `dedicated_cpus`，得到以下结果：

| 指标 | shared p99/max | partitioned p99/max | p99 改善 | max 改善 |
| --- | ---: | ---: | ---: | ---: |
| direct virtual timer injection -> guest IRQ | 2,275,000 / 4,947,833 ns | 2,288,416 / 4,960,958 ns | -0.590% | -0.265% |
| periodic jitter | 34,459 / 34,459 ns | 34,458 / 34,458 ns | +0.003% | +0.003% |
| dispatch latency | 43,750 / 43,750 ns | 41,708 / 41,708 ns | +4.667% | +4.667% |
| timerfd proxy | 50,958 / 50,958 ns | 50,083 / 50,083 ns | +1.717% | +1.717% |

每侧 direct trace 都包含 249 个 guest IRQ 样本；对应 host trace 各有 374 次成功
注入，零丢样、零 incomplete、零注入失败和零 counter-frequency mismatch。两个
vCPU 的 `pcpu_mask` 分别为 `0x2`/`0x4`，迁移数均为零。idle 场景下两种配置的
pCPU 1/2 busy 比例几乎相同，因此这组数据验证了配置和测量链路，却没有构造出
shared 独有竞争；下一步需要独立、可观测的受控干扰源。

本组 `direct_irq_max_improved_in_this_pair=false` 且
`m2_exit_gate_met=false`。它来自 dirty 工作树，只能作为诊断证据；正式结论仍需
clean commit 上的 5 组 AB/BA、10,000 样本和 soak。

#### 5.4.3 guest CPU-stress direct 调试配对

第二组调试配对保持同一 trace-enabled kernel、DTB、rootfs、双 vCPU 映射和 20 次
采样，只将 StarryOS guest CPU1 置于忙循环：

| 指标 | shared p99/max | partitioned p99/max | p99 改善 | max 改善 |
| --- | ---: | ---: | ---: | ---: |
| direct virtual timer injection -> guest IRQ | 2,196,833 / 4,948,125 ns | 2,200,916 / 4,949,000 ns | -0.186% | -0.018% |
| periodic jitter | 33,333 / 33,333 ns | 33,917 / 33,917 ns | -1.752% | -1.752% |
| dispatch latency | 42,000 / 42,000 ns | 41,708 / 41,708 ns | +0.695% | +0.695% |
| timerfd proxy | 51,125 / 51,125 ns | 60,459 / 60,459 ns | -18.257% | -18.257% |

shared/partitioned 的 pCPU1 busy 为 99.750%/99.796%，pCPU2 busy 为
83.189%/83.229%；vCPU0/1 的 affinity 均为 `0x2`/`0x4` 且零迁移。两侧负载几乎
相同，说明该场景只能验证 guest stress 和 accounting，不能证明隔离。direct p99/max
也都没有改善，因此 `m2_exit_gate_met=false`。

#### 5.4.4 跨 VM noise 调试阻塞项

实验性 AArch64 noise guest 使用同一 80-byte 二进制、固定 24 MHz counter 和 180 秒
忙循环，并通过 PSCI `SYSTEM_OFF` 结束。常规单 guest profile 不加载它；只有显式
`starry-noise-shared/partitioned` profile 才启用 round-robin 和第二 VM。

已依次保留以下失败证据：

1. 读取未初始化的 `CNTFRQ_EL0` 导致运行时间为零；已通过构建时固定 24 MHz 和
   确定性脚本测试修复。
2. 同一 pCPU 上两条 vCPU task 的 host timer 抢占触发 nested-vCPU panic；将单次
   architecture guest run slice 置于 `NoPreempt` 后，nested marker 不再出现。
3. noise affinity `0xa` 允许 pCPU1/3 时，迁移到 pCPU3 后出现 hwirq 26 和
   `ESR_EL2=0x96000007` current-EL data-abort 风暴。
4. shared noise 改为 singleton `0x2` 后，两条 vCPU 均固定 pCPU1、零迁移，但 noise
   首次运行约 9 ms 后仍出现 `ESR_EL2=0x96000021`，随后宿主串口输出被破坏。

因此当前既不能声称 vCPU 迁移安全，也不能声称两个 AArch64 vCPU 可在同一 pCPU
安全轮转。partitioned profile 未继续运行，因为与已失败的 shared 侧拼接不会形成
有效配对。恢复流程均在失败后冷启动并确认 Linux 根分区 `/dev/mmcblk1p2`；这些
日志只用于定位，不参与延迟统计。

#### 5.4.5 AxVisor host-noise 受控干扰调试配对

当前维护路线使用单 StarryOS guest 和独立 AxVisor host task，避免跨 VM vCPU 轮转
故障。两侧都使用 round-robin、同一 busy-loop 和 180 秒安全上限；shared 把 task
固定到 StarryOS vCPU0 所在的 pCPU1，partitioned 把 task 固定到 pCPU3。task 必须在
guest 前 ready，在 guest 完成后以 `guest-complete` 停止，并把精确 affinity 和覆盖
窗口写入持久 host trace。

初版 20-sample 实体 smoke 的 direct IRQ p99/max 改善为 `91.498%/88.171%`，
但 shared console 出现 5,825 次 pCPU1 `Unhandled IRQ ... hwirq 27`。该组只用于
暴露 VM exit 后残留 guest timer source 和同步 UART 热路径混杂，不进入改善统计。

根因修复遵循以下不可交换的顺序：每次 VM exit 先保存 guest timer 状态；若本次是
timer IRQ exit，则先让 GIC 应答并把物理 PPI 转入 hardware LR；随后在 current-vCPU
作用域清除和宿主调度前关闭本地 `CNTP/CNTV`；下一次 entry 恢复保存的 `CVAL/CTL`。
把关闭动作放在 GIC 应答前会撤销 level PPI，实体板曾因此停在 guest IRQ 初始化，
现已由确定性顺序回归和实体 smoke 同时覆盖。

修复后按 AB/BA/AB 预定顺序完成了三组 20-sample 实体 smoke：

| Pair/order | shared direct p99/max | partitioned direct p99/max | p99 改善 | max 改善 | direct pairs shared/partitioned |
| --- | ---: | ---: | ---: | ---: | ---: |
| 1 / AB | 41,902,000 / 44,061,208 ns | 3,745,291 / 7,597,916 ns | +91.062% | +82.756% | 350 / 265 |
| 2 / BA | 43,193,500 / 49,487,083 ns | 3,751,708 / 7,806,458 ns | +91.314% | +84.225% | 349 / 266 |
| 3 / AB | 41,842,791 / 53,339,125 ns | 3,728,083 / 7,655,958 ns | +91.090% | +85.647% | 349 / 265 |

六次 host-noise 观测 mask 均为预期的 shared `0x2`、partitioned `0x8`，覆盖完整
host trace 并以 `guest-complete` 结束；snapshot 均通过只读 fsck，vCPU 均零迁移，
无 `ESR_EL2`、panic、nested-vCPU、未处理 IRQ 或样本缺失。所有 host trace 的
`dropped`、`incomplete`、`failed_injections`、`counter_frequency_mismatches` 和
`unowned_virtual_timer_irqs` 均为零；热路径不再同步打印，通用未处理 IRQ 日志也仅
按累计次数为 2 的幂时限流输出。板卡 gate 只接受 `AXVISOR_SNAPSHOT_SYNC_OK`，并在
确认文件系统同步后自动冷启动回 Linux。

三组 direct IRQ p99/max 改善方向一致，故调试用重复 smoke 门达到 3/3。每个比较器
仍正确输出 `m2_exit_gate_met=false`：当前证据来自 dirty worktree 且每项仅 20 个
用户态样本，clean-commit 正式矩阵仍为 0/5。下一步冻结当前实现、schema、阈值和
AB/BA 顺序，在 clean commit 上执行 5×10,000 和 shared/partitioned 30 分钟 soak。

### 5.5 M2 退出条件

- [x] 调试前置门：单 guest host-noise 的实现、强度、实际 placement、停止原因和 trace 覆盖已在一组实体配对中验证。
- [x] PPI27 测量扰动门：每次 VM exit 在 GIC 应答后暂停 guest timer，下一次 entry 恢复；修复后实体配对的 unowned/未处理 timer IRQ 与同步热路径输出均为零。
- [x] 重复 smoke 门：固定配置的 3 组 AB/BA 中 direct IRQ p99/max 改善方向一致（AB/BA/AB，3/3；仅作为 dirty-worktree 调试门）。
- [ ] 任务一的正式客户机是 StarryOS，且至少有 2 个 vCPU。
- [ ] idle、stress 和 soak 均有 raw samples。
- [ ] 主要指标 p99 不退化超过 5%。
- [ ] worst-case 在至少 4/5 配对运行中下降。
- [ ] 汇总最大值相对 shared 至少改善 10%。
- [ ] 无丢样、死锁、客户机重启或调度饥饿。
- [ ] 受控干扰源在两侧具有相同实现/强度，实际 placement 与预设一致，且运行中无 nested-vCPU、current-EL exception 或 vCPU 迁移错误。
- [ ] 若门槛未满足，继续定位 IRQ affinity、housekeeping、锁竞争或日志路径，不将仅有 p99 改善描述为完成。

## 6. M3：协议故障和重复性证据

### 6.1 ACK 丢失

增加 `fault-ack-loss` profile：

- 使用固定随机种子或固定丢失序列；
- 明确记录配置丢失数和实际丢失数；
- 保存发送、重发、重复接收、ACK 和最终确认序号；
- 验证重发上限、幂等性和正常恢复。

### 6.2 ERROR

增加跨客户机 malformed packet profile，至少覆盖：

- 协议版本错误；
- 消息长度错误；
- CRC 错误；
- 非法消息类型；
- 非法状态转换。

接收方必须返回对应 `ERROR`，并能继续处理下一条正常消息。host/unit 测试继续保留，但不能代替实体跨客户机证据。

### 6.3 重启恢复

增加 `fault-restart` profile：

- 通信进行中重启其中一个客户机；
- 控制器进入安全输出；
- 新 session/epoch 不接受旧 ACK、旧状态或延迟报文；
- 客户机恢复后自动重新建立会话；
- 记录恢复用时和期间丢失的控制周期。

### 6.4 重复次数和统计

- 每个 fault profile 至少 3 次。
- 正式 full manual/native/RKNN NPU 每项至少 5 次；ONNX Runtime CPU 通过 M4-plus 可行性门后同样至少 5 次。
- 每个最终 smoke 镜像至少 3 次。
- 报告 median、IQR、单次最大值和 worst-of-runs。
- 对配对实验同时报告每一对的差值，不能只汇总所有样本后比较。

### 6.5 M3 退出条件

- [ ] ACK 丢失的配置值、观测值、重发和恢复结果一致。
- [ ] 每类 malformed packet 都有实体客户机返回的正确 `ERROR`。
- [ ] 重启后没有旧 session 数据污染新会话。
- [ ] 所有 profile 达到规定重复次数且原始数据完整。
- [ ] 分析器能够拒绝样本缺失、哈希不一致或 marker 不完整的运行。

## 7. M4：同源 ONNX、RKNN NPU 与 ONNX Runtime CPU 推理

### 7.1 技术决策和交付等级

继续使用 `tools/ivcproto/src/neural.rs` 中 `thermal-4x6x1-v1` 的固定 4×6×1 权重：

- 不增加训练数据或训练流水线；
- 不根据调试或正式实验结果重新调整权重；
- 将其描述为“确定性固定权重神经控制器”，不声称训练精度；
- 固定权重只在一个机器可读文件中维护，native 常量和 `.onnx` 均由它生成或接受逐元素契约校验；
- `.onnx` 是唯一跨后端部署模型来源，`.ort` 和 `.rknn` 都必须由它生成，禁止为两个 Runtime 分别维护模型图或权重。

部署关系固定为：

```text
fixed weights
    └── thermal-4x6x1-v1.onnx
        ├── thermal-4x6x1-v1.ort
        │   └── ONNX Runtime CPU EP on StarryOS
        └── thermal-4x6x1-v1-rk3588-fp16.rknn
            └── RKNN Runtime -> StarryOS RKNPU driver -> RK3588 NPU
```

交付分为两层：

| 等级 | 必须完成的结果 | 定位 |
| --- | --- | --- |
| M4-core | ONNX 标准来源、native oracle、RKNN FP16 模型、StarryOS RK3588 NPU 实体闭环 | 正式交付，优先完成 |
| M4-plus | `.ort`、最小 ONNX Runtime CPU EP、StarryOS 实体 CPU 对照闭环 | 增强项，通过可行性门后进入正式矩阵 |

不使用 ONNX Runtime 上游的 RKNPU Execution Provider：其官方支持范围只列出 RK1808 Linux，不包含 RK3588。也不为本项目新写自定义 ORT EP；若使用 `.rknn` 和 `librknnrt.so`，报告必须标记为 `backend=rknn-npu`，不得称为 ONNX Runtime NPU 推理。

官方依据：

- [RKNN-Toolkit2](https://github.com/airockchip/rknn-toolkit2)
- [RKNN Toolkit2 2.3.2 ONNX 算子支持](https://github.com/airockchip/rknn-toolkit2/blob/master/doc/RKNNToolKit2_OP_Support-2.3.2.md)
- [Rockchip WSL2 使用说明](https://github.com/airockchip/rknn-toolkit2/blob/master/doc/Using%20RKNN-ToolKit2%20in%20WSL.md)
- [ONNX Runtime RKNPU EP 支持范围](https://onnxruntime.ai/docs/execution-providers/community-maintained/RKNPU-ExecutionProvider.html)

### 7.2 模型和工具链产物

计划新增：

```text
competition/ivc/model/
├── README.md
├── export_thermal_onnx.py
├── convert_thermal_ort.py
├── convert_thermal_rknn.py
├── verify_thermal_models.py
├── requirements-lock.txt
├── thermal-4x6x1-v1.weights.json
├── thermal-4x6x1-v1.onnx
├── thermal-4x6x1-v1.ort
├── thermal-4x6x1-v1-rk3588-fp16.rknn
├── required_operators_and_types.config
├── golden-vectors.json
└── model-manifest.json
```

标准模型图保持为：

```text
[1, 4] input -> Gemm -> Relu -> Gemm -> Clip(0, 1) -> [1, 1] output
```

输入合法性检查、归一化和最终执行器取整继续保留在 Rust 中。导出器和 manifest 固定：

- ONNX opset、tensor 名称、shape 和 dtype；
- producer/version metadata、权重顺序和字节表示；
- canonical weights、native 生成结果、`.onnx`、`.ort`、`.rknn` 和 operator config 的 SHA-256；
- Python、ONNX、ONNX Runtime、RKNN Toolkit、RKNN Runtime 及转换脚本版本；
- RKNN target、精度、量化开关和所有非默认转换参数；
- 许可证、下载来源和允许再分发范围。

若转换器为文件加入不可重复 metadata，验收以固定工具环境、graph/initializer 语义、模型检查结果和每次产物哈希共同判定；不得因此忽略权重或图结构变化。

### 7.3 WSL2 确定性转换环境

WSL2 Ubuntu 22.04 继续作为唯一模型构建主机。仓库脚本必须能从干净 WSL2 环境完成以下流程：

1. 创建隔离 Python 环境并按 hash 安装锁定依赖；
2. 从 canonical weights 生成或校验 Rust 常量并导出 `.onnx`，运行 ONNX checker；
3. 使用与 Runtime 兼容的固定 RKNN Toolkit2 版本执行 `load_onnx`；
4. 固定 `target_platform=rk3588`、`do_quantization=False`，生成非量化 FP16 `.rknn`；
5. 使用固定 ONNX Runtime 版本生成 `.ort` 和 reduced operator/type 配置；
6. 生成 golden vectors、manifest 和全部哈希；
7. 第二次在全新临时目录重建，并比较语义和产物记录。

RKNN Toolkit2 2.3.2 是首个候选版本，但只有在它与仓库内 `librknnrt.so`、StarryOS RKNPU ioctl ABI 和实体板驱动共同通过兼容性 spike 后才冻结。不得只升级转换器而不记录或验证 Runtime/driver 组合。厂商 wheel 或二进制库若不允许直接纳入仓库，则保存官方来源、版本、SHA-256 和自动下载/校验步骤，不复制未授权文件。

本阶段不做 INT8 量化。若 FP16 无法满足功能或时限，INT8 校准必须作为单独计划评审，不能借“转换”名义引入训练或未冻结数据集。

### 7.4 后端和 StarryOS 集成边界

定义小型后端能力接口：

```text
InferenceBackend
├── NativeDenseBackend       backend=native
├── OrtCpuBackend            backend=onnxruntime-cpu
└── RknnNpuBackend           backend=rknn-npu
```

- `NativeDenseBackend` 保留现有纯 Rust `f32` 实现，作为模型语义 oracle 和显式恢复路径。
- `OrtCpuBackend` 使用固定 `onnxruntime_c_api.h` 的小型 FFI wrapper；只启用 CPU EP、sequential execution、单线程和关闭 spinning。
- `RknnNpuBackend` 使用固定 `rknn_api.h` 和现有 `librknnrt.so`，复用 `apps/starry/orangepi-5-plus-uvc-rknn` 已有用户态加载方式以及 StarryOS `/dev/dri/card1` RKNPU ioctl 链路。
- 不把 ONNX Runtime 或 RKNN 依赖传播到协议 crate 或可复用 `no_std` crate。
- 每个后端只创建一个长期 session/context；模型、输入输出 tensor 和工作区在控制循环开始前加载并预分配。
- NPU core mask 在正式实验前通过单核/多核 spike 冻结；之后不得按单次结果调整。
- 当前 `rockchip-npu` 直接 operation API 主要覆盖 INT8 MatMul，不用它手写完整模型图，也不绕过 RKNN 编译器拼寄存器命令。

禁止静默回退。加载、初始化或推理失败时本次运行直接失败；只有用户显式选择 `backend=native` 才运行 native。每次结果必须记录：

- backend、模型 ID、ONNX 来源 SHA 和部署模型 SHA；
- 实际精度、输入输出 dtype/shape；
- ORT 或 RKNN Runtime 版本、RKNPU 驱动版本和 core mask；
- 初始化结果、推理次数、Runtime 错误码和 driver submit 证据。

### 7.5 分阶段验证阶梯

#### 7.5.1 Stage A：无板模型验证

1. 导出器单元测试、ONNX checker 和 initializer 对照。
2. host ONNX Runtime 运行 golden vectors。
3. native/ORT 对随机、边界和执行器取整阈值附近输入做 10,000 组差分。
4. RKNN Toolkit2 编译 `.onnx`；保存完整转换日志和算子映射，确认 `Gemm`、`Relu`、`Clip` 没有落入 custom CPU op。
5. 使用 RKNN 模拟器或官方精度分析能力对固定 corpus 做首次差分；模拟器结果不能替代实体 NPU。

若 RKNN 编译器因 shape/alignment 拒绝原图，只允许在不改变权重和数学语义的前提下把 `Gemm` 展开为 `MatMul + Add`，或添加可证明等价的 padding/slice。任何图改写都必须回到 native/ONNX golden vectors 重新验证并写入 manifest。

#### 7.5.2 Stage B：Linux 实体参考

1. 在同一 OrangePi 5 Plus 的 Linux 环境用选定 `librknnrt.so` 加载 `.rknn`。
2. 连续执行至少 10,000 次固定和随机输入，记录输出、错误、初始化时间和 steady-state 延迟。
3. 查询 `RKNN_QUERY_PERF_RUN`，记录 Runtime/driver 版本和实际 core mask。
4. Linux 结果与 native oracle 通过数值和执行器一致性门后，才进入 StarryOS。

Linux 参考只用于区分模型/Runtime 问题与 StarryOS ABI/driver 问题，不计作 StarryOS 比赛证据。

#### 7.5.3 Stage C：StarryOS 离线 NPU 验证

1. 以 `rknpu` feature 构建 rootfs，加载与 Linux 参考相同的模型和 Runtime。
2. 首先运行单输入 smoke，再连续推理 10,000 次。
3. 同时保存用户态 wall-clock、`RKNN_QUERY_PERF_RUN` 和 StarryOS RKNPU submit/错误计数。
4. 验证确有 NPU submit，且没有 custom CPU fallback、driver error、NaN、内存泄漏或模型重载。
5. 重启板卡后至少重复 3 次 smoke，证明模型加载与设备初始化可重复。

#### 7.5.4 Stage D：StarryOS + Zephyr 闭环

1. `native`、`rknn-npu` 分别完成同一 smoke；M4-plus 通过后再加入 `onnxruntime-cpu`。
2. 使用相同控制周期、输入序列、网络 profile、rootfs 基线和实验时长。
3. 正式 full 采用轮换顺序，避免总让某个后端处于冷板或热板状态。
4. 每个正式后端至少完成 5 次，并保存逐样本模型输入、浮点输出、执行器命令、状态回传和端到端延迟。
5. 分析器从 raw data 验证实际 backend、重复次数、模型哈希和零静默回退。

### 7.6 数值、实时性和资源验收

数值门分开定义，不能把 `f32` 与 NPU FP16 混为一个阈值：

| 对照 | 最大绝对误差 | 最终执行器命令 |
| --- | ---: | --- |
| native vs ONNX Runtime CPU | `<= 1e-6` | 100% 一致 |
| native vs RKNN FP16 | 初始门 `<= 1e-3` | 100% 一致 |

RKNN FP16 阈值必须在正式实验前由固定 10,000 组 corpus 冻结。若失败，不得根据正式结果放宽；应定位图转换、dtype、输入布局、Runtime 版本或执行器边界问题。任何 backend error、NaN、Inf、shape mismatch 或命令不一致都使该次运行失败。

延迟分别报告：

- cold initialization：动态库加载、context/session 创建和模型加载；
- warm-up：固定次数且不混入正式 steady-state 分位数；
- inference wall time：包含 inputs set/copy、run、outputs get/copy；
- device time：RKNN Runtime 可查询到的 NPU 执行时间；
- control end-to-end：输入获取、推理、网络发送、RTOS 动作和状态返回。

正式预算：

- steady-state inference p99 小于 100 ms 控制周期的 10%；
- 单次最大 inference wall time 小于控制周期的 20%；
- 零 missed control deadline；
- 连续 10,000 次无增长型内存泄漏；
- 记录二进制、动态库、模型、rootfs 大小和内存高水位；rootfs 保留至少 20% 空间。

4×6×1 模型只有约 30 次乘加，NPU 的提交和同步开销可能大于 CPU 计算。M4-core 要求证明“实际 NPU 执行且满足控制周期”，不预设 NPU 比 native/ORT 更快；只有 wall-clock 数据确实改善时才使用“加速”表述，否则报告为硬件卸载、驱动集成和扩展性证据。

### 7.7 两条可行性门和停止条件

#### 7.7.1 M4-core：RKNN NPU 门

必须同时满足：

- 固定 ONNX 图可被 RKNN 编译器接受，所有关键算子位于 NPU 图；
- converter、`librknnrt.so` 和 StarryOS RKNPU ioctl ABI 版本兼容；
- Linux 与 StarryOS 均能稳定加载同一 `.rknn`；
- StarryOS 有可核验的 NPU submit/device-time 证据；
- 数值、执行器命令、deadline、内存和重复性门全部通过。

若模型只能依赖未授权二进制、只能在 Linux 工作、实际落入 CPU、持续错过 deadline，或经过等价图改写仍不能在 RK3588 编译/运行，则记录 M4-core no-go，保留 native 后端，并明确不得把初始化成功描述为 NPU 推理完成。

#### 7.7.2 M4-plus：ONNX Runtime CPU 门

先完成可行性 spike，确认：

- 固定 release/commit 的 AArch64 musl minimal build 可复现；
- `.ort`、reduced operator/type config 与 Runtime 版本严格匹配；
- 所需线程、futex、mmap、时间和文件接口可由 StarryOS 满足；
- 最小模型加载不触发大范围未实现 Linux ABI；
- 性能和镜像尺寸不破坏控制周期与部署流程。

少量、语义明确的 syscall 修复按确定性回归测试流程实现；若需要大范围模拟 Linux 用户态、无法稳定链接或持续错过控制周期，则记录 M4-plus no-go，不阻塞 M4-core 和 P0/P1。host/QEMU 上的 ORT 成功只能标记为部分完成，不能替代实体 StarryOS 结果。

### 7.8 M4 执行顺序

1. M4-0：冻结 canonical weights、native 生成/校验规则、输入归一化、执行器规则和 golden corpus。
2. M4-1：实现确定性 ONNX 导出、manifest 和二次重建验证。
3. M4-2：完成 RKNN FP16 转换及算子/许可证审计。
4. M4-3：完成 Linux RKNN reference 和 StarryOS 离线 NPU 10,000 次验证。
5. M4-4：完成 StarryOS + Zephyr `rknn-npu` smoke/full 闭环。
6. M4-5：并行完成 ORT CPU 构建 spike；通过门后加入 StarryOS 离线及闭环验证。
7. M4-6：冻结所有版本/哈希，在 clean commit 上执行正式轮换矩阵并更新报告。

### 7.9 M4 退出条件

M4-core 必须完成：

- [ ] canonical weights、`.onnx`、`.rknn`、golden vectors 和 manifest 可由固定 WSL2 工具链重建。
- [ ] RKNN 转换日志证明关键算子进入 NPU 图，无 custom CPU fallback。
- [ ] native/RKNN 10,000 组差分满足 FP16 和执行器一致性门。
- [ ] Linux reference 与 StarryOS 离线 NPU 验证均通过。
- [ ] StarryOS 实体板完成 `backend=rknn-npu` full 闭环 5 次。
- [ ] 结果包含 Runtime/driver/core mask、NPU submit/device-time 和模型哈希证据。
- [ ] 推理延迟、deadline、内存、rootfs 和重复性满足预算。

M4-plus 增强项：

- [ ] `.ort`、operator config 和 ORT 构建可由固定工具链重建。
- [ ] native/ORT 10,000 组差分满足 `f32` 和执行器一致性门。
- [ ] StarryOS 实体板完成 `backend=onnxruntime-cpu` full 闭环 5 次；否则保存明确的 no-go 证据。
- [ ] 所有后端均无静默回退，raw data 与 metadata 记录的实际 backend 一致。

## 8. 正式实验总矩阵

| 类别 | 配置 | 正式次数 |
| --- | --- | ---: |
| RT | shared idle | 5 |
| RT | partitioned idle | 5 |
| RT | shared guest CPU1 stress | 5 |
| RT | partitioned guest CPU1 stress | 5 |
| RT | shared controlled interference | 5 |
| RT | partitioned controlled interference | 5 |
| RT | shared/partitioned interference soak | 各 1 次、每次 >= 30 分钟 |
| 控制 | Starry manual full | 5 |
| 控制 | Starry neural native full | 5 |
| 控制 | Starry neural RKNN NPU full | 5 |
| 控制 | Starry neural ONNX Runtime CPU full | 5（M4-plus 通过后） |
| 故障 | ACK loss | 3 |
| 故障 | malformed/Error | 3 |
| 故障 | restart recovery | 3 |
| 健康检查 | 每个最终 smoke 镜像 | 3 |

正式结果必须按运行顺序保存，失败运行不得删除。允许修复后建立新 result set，但必须保留失败原因和废弃标记。manual/native/RKNN/ORT 的正式顺序使用轮换表预先固定，并在 metadata 中保存，避免温度和先后顺序偏差。

## 9. M5：报告、复现和视频

更新：

- `competition/test-report.md`：只引用通过本计划验收门的正式结果；
- `competition/reproduce.md`：给出从干净 WSL2 到实体板结果的单命令流程；
- `competition/design.md`：描述 StarryOS 任务一、直接 IRQ 测量、同源 ONNX 以及 native/ORT CPU/RKNN NPU 三后端边界；
- `competition/video-storyboard.md`：展示任务一、manual/neural、协议故障、实际推理 backend/version 和 NPU submit/device-time marker；
- `competition/results/`：保存压缩 raw data、summary、metadata、checksum 和简短 README。

视频至少展示：

1. WSL2 启动自动化任务；
2. OrangePi 5 Plus/AxVisor/StarryOS 启动标志；
3. StarryOS 任务一 shared/partitioned 对比；
4. StarryOS manual 与 neural 同板对比；
5. ACK 丢失、ERROR 和重启恢复；
6. `backend=rknn-npu`、ONNX/RKNN SHA、RKNN Runtime/driver/core mask 和实际 NPU 执行证据；
7. 若 M4-plus 通过，展示 `backend=onnxruntime-cpu`、ORT 模型 SHA 和 Runtime 版本；
8. 分析器从 raw data 生成最终 summary。

源码 PR 继续按照 `competition/requirement.md` 的当前安排暂缓，不在本计划执行阶段自动创建。

## 10. 最终完成定义

- [ ] 任务一、二、三均存在 StarryOS 实体板结果。
- [ ] manual 和 neural 基线来自同一实体平台和同源配置。
- [ ] 实时测试证明 p99 和 worst-case 同时改善。
- [ ] ERROR、ACK 丢失和重启恢复均有实体跨客户机原始证据。
- [ ] 正式结论达到规定重复次数，并报告跨运行 worst-case。
- [ ] 固定权重可由 WSL2 确定性导出为 ONNX，并同源生成经验证的 RKNN 部署模型。
- [ ] RKNN Runtime 通过 StarryOS RKNPU 驱动在 RK3588 NPU 完成 5 次 full 闭环，并有实际 submit/device-time 证据。
- [ ] ONNX Runtime CPU 在 StarryOS 实体板完成 M4-plus full 闭环；若可行性门失败，保留明确 no-go 证据且不冒充已完成。
- [ ] 所有正式结果对应干净 commit、完整哈希和可验证原始数据。
- [ ] WSL2 可以通过仓库内脚本自动重现部署、运行、恢复和结果回收。
- [ ] 报告、复现文档和视频与最终数据一致。
