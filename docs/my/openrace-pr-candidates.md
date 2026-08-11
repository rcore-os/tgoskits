# OpenRace 独立 PR 候选审核

> 更新时间：2026-08-10
> 基准：upstream/dev @ e04a3ca28
> 工作分支：`openrace/small-fixes-review`（基于最新 upstream/dev，不影响 `openrace/realtime-virq-ab` 主工程）
> 目的：从 68 个本地提交中拆出可独立提交的通用修复，供二次审核后逐一开 PR

---

## 候选 1：fix(arm-vcpu): stop double-advancing HVC/SMC exception PC

| 项 | 内容 |
|---|---|
| 提交 | `f837d3ad0`（新分支上为 `a50b77a0b`） |
| 影响文件 | `virtualization/arm_vcpu/src/architecture/exception.rs`（1 文件，+12/-19，含测试） |
| 上游现状 | **仍存在该 bug**：上游 SMC64 分支仍 `elr + exception_next_instruction_step()` 推进 PC；`handle_hvc_psci_version`/`handle_hvc64_exception` 同样多跳一次 |
| 修复逻辑 | QEMU 与 ARM 架构已把 ELR_EL2 置为 trapping 指令的下一条，trap 层不应再 +4。多跳会导致 PSCI CPU_ON 后跳过紧邻指令，x9 陈旧、后续 store 触发 FAR=0 异常（双 vCPU 启动失败的直接原因） |
| 验证 | `cargo clippy -p arm_vcpu` 通过（aarch64 target）；单元测试断言 PC 不再推进 |
| 风险 | 低。单文件、语义清晰、与上游无冲突 |
| 结论 | **可提交** |

---

## 候选 2：fix(axvm): relocate identity-mapped ramdisk with kernel

| 项 | 内容 |
|---|---|
| 提交 | `507e4c166` 精简后（新分支上为 `44853bc03`，已剔除 bootargs/docs/toml 部分） |
| 影响文件 | `virtualization/axvm/src/config.rs`（+8：`relocate_ramdisk_image`）、`virtualization/axvm/src/vm/boot.rs`（+72：identity 内存下的 ramdisk 跟随 kernel 重定位 + 2 个测试） |
| 上游现状 | bootargs 写入上游已实现（`patch_chosen(explicit_cmdline)`）；**ramdisk 重定位缺失** |
| 修复逻辑 | identity 映射时配置 GPA 会被动态 HPA 替换，kernel 已重定位，但 ramdisk 与 FDT `/chosen` initrd 范围仍指向旧基址，导致固定 load 地址失效。按 kernel 相同的基址偏移重定位 ramdisk |
| 验证 | `cargo test -p axvm --features host-test`：97 通过 0 失败，含新增 `identical_memory_relocates_ramdisk_with_kernel` 与 `non_identical_memory_keeps_ramdisk_address`；clippy 通过 |
| 风险 | 中。仅 identity 映射路径生效，非 identity 行为不变（有测试覆盖） |
| 结论 | **可提交** |

---

## 候选 3：fix(arm-vgic): track guest GICR_WAKER state（需移植）

| 项 | 内容 |
|---|---|
| 来源 | `f545ccd41` 中的 WAKER 增量部分（GICR_TYPER_LAST 部分上游已修复，剔除） |
| 影响文件 | 需移植到 `virtualization/arm_vgic/src/redistributor/mmio.rs`（新结构） |
| 上游现状 | `GICR_WAKER | GICR_SYNCR` 读返回 0、写忽略（mmio.rs:87/134），**无状态跟踪** |
| 修复逻辑 | 跟踪 guest 写入的 `GICR_WAKER`：初始为 `PROCESSOR_SLEEP | CHILDREN_ASLEEP`；读返回保存值；写时 `PROCESSOR_SLEEP` 置位则 `CHILDREN_ASLEEP` 跟随置位（ARM GICv3 语义：子 redistributor 睡眠状态由处理器睡眠派生） |
| 验证 | 未做（尚未移植到新结构） |
| 风险 | 中。需确认新结构 redistributor 状态字段位置与测试框架；行为变化：guest 首次读 WAKER 不再恒为 0 |
| 结论 | **待移植 + 验证后提交** |

---

## 备选（未列入三候选）

| 改动 | 说明 | 状态 |
|---|---|---|
| ArceOS 编译期静态 IP（`cd19795bf` 中 `axruntime/devices.rs` 部分） | 通过 `AX_NET_IP` 等 env 注入静态 IP；API（`StaticIpConfig`/`InterfaceMatcher`）存在、`option_env!` 有先例（AX_LOG），但属任务二辅助工具，需按上游风格打磨 | 暂缓 |

---

## 明确不提交

| 项 | 原因 |
|---|---|
| GICR_TYPER_LAST（20e83803a / f545ccd41 部分） | 上游 `redistributor/mmio.rs:240` 已实现（#1717 重构时） |
| PSCI in guest FDT（428c187dd） | 上游 `f56b496fe` 已合入（#1926，8/9） |
| vCPU startup barrier（415cc4109） | 上游已统一为 VM-wide queue |
| bootargs 写入（507e4c166 部分） | 上游 `patch_chosen` 已有 `explicit_cmdline` |
| arm-vcpu PC advance 早期版本（3a9b58a85） | 错误方向（主张 ELR 指向 trapping 指令），被候选 1 纠正 |
| vIRQ queue/dispatcher/ops 重排队 + GIC retry 系列 | 任务一实时改造主体，主 PR 内容，不可拆 |
| task2 网络基建（udpecho/initramfs/net-dual-guest 配置） | 任务二主体；配置可随主 PR 或单独 test PR |
| 测试基建（zephyr-soft-virq*/periodic/stats） | 依赖 vIRQ 运行时，feature 合入后单独提 test PR |
| `docs/my/*`、`docs/meeting/*`、`results/*`、`001.md`、`dp-001.md`、`REAL_WORK.txt` | 规划/证据文件，不进任何 PR |

---

## 建议提交顺序

1. 候选 1（arm_vcpu，已就绪）→ 单独 PR，快
2. 候选 2（ramdisk，已就绪）→ 单独 PR
3. 候选 3（GICR_WAKER，移植后）→ 单独 PR
4. 主 PR（任务一 vIRQ 改造）→ 大 PR，含测试基建
5. 任务二 PR（网络 + 静态 IP 按需并入）

每个 PR 提交前：`git rebase` 到最新 upstream/dev，跑 clippy + 相关测试，确认无冲突。
