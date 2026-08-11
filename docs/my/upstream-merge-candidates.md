# 可并入上游主线的问题清单

> 日期：2026-08-10
> 分支：`openrace/realtime-virq-ab`；对照：`upstream/dev`（merge-base = `e04a3ca28`，2026-08-09）

## 1. 背景

分支相对 `upstream/dev` 领先 41 个提交（`upstream/dev..HEAD`），上游领先 0 个——**没有任何分支提交进入上游主线**。

核验结论（2026-08-10 逐项比对）：

- 上游独立实现了 per-vCPU dispatch queue（#1661/#1679/#1910），先于分支的 `f298ee57b`——"per-vCPU 队列"方向不是分支首创；
- 上游 `arm_vgic` 已实现 software pending + maintenance（`requeue_software`）；
- 分支真正存活的增量 = axvm 1263 行 + arm_vcpu 120 行（`git diff upstream/dev HEAD --stat`）。

## 2. 存活增量总览

| 文件 | 增量 | 内容 |
| --- | --- | --- |
| `architecture/ops.rs` | +288 | GIC busy 重试、defer injection、外部中断 token |
| `runtime/vcpus.rs` | +246 | park-safe 通知/等待、E1 计数器、周期注入器 |
| `vm/mod.rs` | +218 | 定向 `notify_vcpu`（per-vCPU wait queue + `notify_one`） |
| `runtime/trace.rs` | +203 | realtime trace（实验观测，不推） |
| `runtime/queue.rs` | +118 | 有界队列 `try_push`、`pop_if`（保留阻塞边沿） |
| `runtime/dispatcher.rs` | +89 | retry slot、`has_pending` 门控 |
| `runtime/mod.rs` | +8 | — |
| `arch/aarch64/gic.rs` 等 | +31 | busy/retry 前置检查 |
| `irq/model.rs` | +70 | 注入失败语义 |
| `config.rs`/`axvmconfig` | +36 | `advance_hvc_smc_pc` 配置 |
| `arm_vcpu`（exception/vcpu/types） | +120 | HVC/SMC PC advance 可配置化 |

## 3. 建议合并的 PR（按顺序）

### PR 1：HVC/SMC exception PC advance 平台可配置化

- **提交**：`f837d3ad0`（修复双 +4）+ `67bbad718`（可配置化）
- **上游问题**：`arm_vcpu` 对 HVC/SMC 无条件 `advance_aarch64_exception_pc`（+4）。QEMU（符合 ARM DDI 0487）的 ELR_EL2 已指向下一条指令，再 +4 会跳过一条指令；Phytium Pi 等物理平台 ELR 停在 trap 指令本身，必须 +4（否则 guest 重执行 hvc，PSCI boot 卡死，phytiumpi-linux 板 CI 曾复现）。
- **分支方案**：per-VM 配置 `advance_hvc_smc_pc`（默认 `true` 保持物理平台旧行为），QEMU 配置显式设 `false`。贯穿 `axvmconfig → AxVMConfig → ArmVcpuCreateConfig → handle_exception_sync`。
- **规模**：~120 行（arm_vcpu + axvmconfig + axvisor config），已含单元测试（两种模式）。
- **验证**：QEMU 上 Zephyr/Linux guest 双核启动；phytiumpi-linux 板 CI 不回归。
- **风险**：低。默认值向后兼容。

### PR 2：vIRQ 队列有界化 + 显式 overflow 错误

- **提交**：`7c369edca` 系列中的 queue 部分（`try_push`、`VCPU_INTERRUPT_QUEUE_CAPACITY=64`）
- **上游问题**：`VcpuInterruptQueue::push` 无容量检查 → 每 vCPU `Vec` 无界静默增长。注入过快/消费停滞时内存无限增长且无观测。
- **分支方案**：`try_push` 返回容量错误，`dispatcher.enqueue` 转成 `resource_unavailable`（`QueueOverflow` trace 可观测）。
- **规模**：~80 行（queue.rs + dispatcher.rs）。
- **验证**：overflow 单测；积压场景（周期 <100µs 或人为停 vCPU）下行为分叉记录。
- **风险**：中低。需与上游确认无界是否是设计意图；分支的 retry slot（PR 3）保证"有界不丢边沿"，可一并论证。

### PR 3：GIC 注入失败重试 + retry slot（注入不丢边沿）

- **提交**：`7c369edca`（pop-if 逐个弹出）、`d2b4061e8`（可重试失败上报）、`cb5c0ae13`（per-vCPU retry slot）、`84611259c`（retry 边沿计入 pending）、`35445a6c8`（LR 耗尽 defer）、`cebe19ae5`（GICv2 busy 前置检查）、`7e057a0a3`（只重试瞬时 busy，丢弃终结错误）
- **上游问题**：`arch/aarch64/gic.rs` 无 busy/LR 耗尽处理，drain 期间 GIC busy 的中断可能直接丢失。
- **分支方案**：逐个 pop → 注入失败（瞬时 busy/LR 满）→ 存入 per-vCPU retry slot（队列外，避免与并发生产者竞争）→ `has_pending` 计入 → 下次入口重试；终结错误不再重试。
- **规模**：~250 行（ops.rs + dispatcher.rs + irq/model.rs + gic.rs）。
- **验证**：retry 单测（LR 释放后边沿恢复）；双流 300/300 无丢失。
- **风险**：中。GIC busy 语义需要上游认可；改动跨 4 个文件，建议与 PR 2 合并为一个"注入可靠性"PR。

### PR 4（可选）：定向唤醒 `notify_vcpu` + per-vCPU wait queue

- **提交**：`f298ee57b`（接线）+ `041210b5f`（park-safe + 定向唤醒主体）+ E1 证据（`00844b74c` 等）
- **上游问题**：`dispatch_vcpu_interrupt` 用 `notify_all()` 广播，多 vCPU 下唤醒无关 vCPU。
- **分支方案**：per-vCPU wait queue map + `notify_one` + `has_pending` 门控；条件 wait 避免 park 竞态。
- **证据**：E1 实验 vCPU0 被无关唤醒 124 → 0。
- **规模**：~200 行（vm/mod.rs + vcpus.rs），与上游 vcpus.rs 耦合深。
- **策略**：在 PR 1-3 被接受后再提出，附 E1 数据；若审阅者对设计方向有异议则放弃。

## 4. 不建议推上游（仅实验用）

- `runtime/trace.rs`（realtime trace，+203）——实验观测工具；
- 周期注入器 `spawn_periodic_virq_injector`、E1 计数器 `notify_woke_count`——实验装置/指标；
- guest 资产（zephyr-soft-virq/-suspend/periodic 的 main.c、TOML、统计脚本）。
- 例外：`c26a76ed8` 的 vCPU id 边界检查可剥离为通用安全检查。

## 5. 已在上游 merge 中死亡（不要盲推）

| 内容 | 现状 |
| --- | --- |
| GICR_TYPER_LAST 修复（`20e83803a`） | 提交在历史中，但上游 arm_vgic 重构后旧 `v3/vgicr.rs` 不存在。需先验证新 redistributor 是否已正确设置 TYPER，大概率已修。 |
| gppt emulated-GIC 设备（`gppt-gicd`/`gppt-gicr`） | 整个设备模型被上游删除。 |

## 6. 执行顺序与前提

1. **前提（0.5 天）**：迁移 4 个实验配置 TOML 到新 schema（`guest_type`、`[devices] passthrough/disabled`），跑通 Zephyr guest——所有 PR 的本地验证依赖它。
2. **PR 1（1-2 天）**：HVC/SMC 可配置化，最干净先推。
3. **PR 2+3（2-3 天）**：合并为"注入可靠性"PR（有界 + 重试 + 不丢边沿）。
4. **PR 4（2-3 天，视上游反馈）**：定向唤醒隔离。
5. 每个 PR 推前 rebase 到最新 `upstream/dev`（当前 merge-base 为 2026-08-09）。

## 7. 参考材料

- 进度文档：`docs/my/openrace-realtime-progress.md`（B 改动与实验结果）
- 下一步分析：`NEXT.md`（双 vCPU、跨 vCPU 定向唤醒、长稳压力、统计可信度）
- 遗留现场：REAL_WORK.txt 中 vGICR ICENABLER 直写问题（gppt 已删除，新架构下自然消失）
