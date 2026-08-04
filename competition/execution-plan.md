# Competition Execution Plan

> 状态：执行中
>
> 基线日期：2026-08-04
>
> 依据：[requirement.md](requirement.md) 与 [improvement-plan.md](improvement-plan.md)
>
> 自动化主机：WSL2
>
> 实体平台：OrangePi 5 Plus（RK3588）
>
> Guest 组合：AxVisor + StarryOS + Zephyr

## 1. 最终目标

在同一块 OrangePi 5 Plus 上形成可由 WSL2 单命令重复执行的完整比赛证据链：

1. StarryOS 完成任务一、二、三，不再只以 Linux/QEMU 结果支撑核心结论。
2. 实时性结果同时给出改造前后、空载与压力、p99 与 worst-case，并保留原始样本。
3. StarryOS manual 与 neural 使用同板、同配置、同工作负载进行正式配对。
4. ACK 丢失、ERROR、客户机重启恢复均有至少 3 次实体跨客户机证据。
5. 固定 4×6×1 权重不训练、不调参，确定性导出 ONNX；由同一 ONNX 生成 RKNN NPU 模型和 ONNX Runtime CPU 对照模型。
6. WSL2 自动完成构建、部署、冷启动、串口采集、分析、Linux 恢复、fsck 和结果同步。

## 2. 当前状态与剩余关键路径

| 工作包 | 当前状态 | 下一验收点 |
| --- | --- | --- |
| WSL2 板卡自动化 | 已打通 | 保持单命令运行、失败留档和 Linux 自动恢复 |
| StarryOS 实体任务一/M2 | 正式受控干扰 5 对、双 soak、CPU1 stress 5 对已完成 | 在最终报告中严格限定改善结论的适用场景 |
| ACK loss | 正式 3/3 已完成 | 汇入最终报告 |
| ERROR | 正式 3/3 已完成 | 汇入最终报告 |
| restart recovery | `6adf49e09` 实体正式 3/3 已完成，campaign formal gate 为 true | 汇入最终报告并保留 pre-reset deadline miss 不利指标 |
| StarryOS manual/neural full | `f4ced3758` 上正式 5 对、10/10 half 已完成 | 汇入最终报告，同时披露 neural 最大超调退化和无稳定延迟优势 |
| 模型单一来源与 ONNX | M4-0/M4-1 已完成：固定权重、Rust oracle、10,000 vectors、确定性 ONNX 和 manifest 均已验证 | 提交并冻结生成流水线与哈希 |
| RK3588 NPU | 平台/所有权审计已完成；当前 AxVisor guest 尚未获得 NPU 资源 | 完成 RKNN 2.3.2 FP16 转换、Linux reference，再实现安全 guest handoff |
| ONNX Runtime CPU | host CPU EP 10,000 组差分已通过；StarryOS `.ort`/minimal runtime 尚未实现 | 冻结 AArch64 musl 版本与 operator config，通过 M4-plus 门后进入实体矩阵 |
| 报告与视频 | 待最终数据冻结 | 所有正式结果通过聚合门后更新 |

当前执行顺序固定为：

```text
E0 工作区与证据冻结（完成）
  -> E1 restart 证据加固与正式 3/3（完成）
  -> E2 StarryOS manual/neural 正式 5 对（完成）
  -> E3 同源 ONNX 与 RKNN NPU 主路线（执行中）
  -> E4 ONNX Runtime CPU 对照路线
  -> E5 总体验收、报告和视频
```

E0-E2 的正式证据已冻结，不因 M4 结果调整阈值或重跑成功 half。E3/E4 可以共享同一 ONNX 与 golden corpus，但 RKNN 工具链、ORT 工具链和板端 Runtime 必须分别锁定，不能用 host 成功替代实体 StarryOS 结果。

## 3. 全程执行约束

### 3.1 工作区和同步

- Windows 工作区用于与用户共享源码；WSL2 工作区用于构建、测试和板卡自动化。
- 正式实验只能从 clean commit 的独立 worktree 运行，禁止从当前 dirty 工作区直接生成正式结论。
- 每次开始前执行 Windows/WSL2 精确同步检查；每次结束后只同步本任务涉及的源码、配置、文档和结果。
- 不覆盖现有 dirty 修改，不删除失败运行，不改写已经冻结的预注册或 raw data。
- Zephyr 的 ignored build artifact 必须记录来源 commit 和 SHA-256，不能只复制二进制而不记身份。

### 3.2 板卡状态机

- 所有实体运行经仓库内 board runner 和已有电源脚本执行；已授权按脚本自动重启或冷启动，无需逐次询问。
- 启动带 `--restore-linux` 的运行前必须显式设置 `TGOS_BOARD_POWER_CONFIG` 与 `ORANGEPI_POWER_PYTHON`，并在预检中确认配置文件和解释器均可读/可执行。
- 每次运行均须取得 board lease；结束时必须恢复 Linux。
- 写入 StarryOS/结果分区前后执行同步与文件系统健康检查。
- 运行成功至少要求：串口完整、raw 可解析、哈希一致、快照可读、fsck 通过、Linux 根分区恢复为可写状态。
- 任一门失败即保留为失败证据；修复后建立新的 result set 或追加 amendment。

### 3.3 正式证据规则

- 先冻结 clean commit、配置、镜像哈希、运行顺序、次数和验收阈值，再启动正式 campaign。
- 分析器必须从 raw data 重算结论；UART 摘要只能交叉校验，不能替代原始数据。
- 不得通过放宽 marker、样本数、哈希或计数条件来接纳损坏记录。
- paired experiment 报告逐对差值、median、IQR、单次最大值和 worst-of-runs。
- 报告有利和不利指标；已观察到的 neural 最大超调退化不得省略。

## 4. E0：冻结当前实现并建立可复跑基线

### 4.1 待完成动作

- [x] 将本轮 restart UART 加固的精确文件从 Windows 同步到 WSL2 开发 worktree。
- [x] 保持 shell/Python 文件原有可执行位，统一 LF。
- [x] 运行 restart 专项测试、完整 IVC Python 测试、Zephyr 构建和相关 Rust 验证。
- [x] 把 UART 加固作为独立 commit 提交，不混入已有失败证据目录。
- [x] 从该 commit 建立新的 clean physical-run worktree。
- [x] 在 run worktree 中重新生成或复制经哈希核验的 Zephyr restart binary。

本轮加固应覆盖：

- AxVisor restart ARMED/COMPLETE 记录重复输出并允许分析器从 UART 粘连前缀中恢复完整记录；
- StarryOS restart-resume 记录在进入 post-reset 控制前重复输出；
- Zephyr 保存 stale replay 与 recovery 字段，并在安静的收尾阶段重复报告；
- 分析器仍要求至少存在一份完整记录，并继续拒绝真正缺失或冲突的证据。

### 4.2 验证命令

在 WSL2 clean/development worktree 中执行：

```bash
python3 -m unittest \
  competition.ivc.tests.test_analyze_board \
  competition.ivc.tests.test_axvisor_guest_restart_contract \
  competition.ivc.tests.test_qemu_config \
  competition.ivc.tests.test_starry_guest_contract

python3 -m unittest discover \
  -s competition/ivc/tests \
  -p 'test_*.py'

ninja -C competition/ivc/zephyr/build-board-restart

cargo fmt --all
cargo xtask clippy --package axvm
cargo xtask clippy --package arm-gic-driver
cargo xtask clippy --package axbuild
cargo test -p axvm --lib
cargo xtask axvisor build \
  -c competition/ivc/config/axvisor-orangepi-5-plus-restart.toml

git diff --check
```

### 4.3 退出条件

- [x] 所有专项与完整测试通过。
- [x] restart Zephyr binary 已重建且 SHA-256 记录在案。
- [x] 提交后的 run worktree 为 clean，`git status --short` 为空。
- [x] 未覆盖或删除已有用户修改和失败结果。

冻结记录（2026-08-04）：

- UART 终端证据错峰输出与重放加固提交为 `c41e222252654392fed02b1b22f3d5811dd6962c`；UART SHA 片段一致性校验提交为 `6adf49e09ce91b53d2573cb8d34c60dc6a9ec47c`。
- 完整 IVC Python 测试共 110 项通过；Zephyr host logic、`ivcproto` lib/bin 测试、目标 crate clippy、全仓 rustfmt 与 diff check 均通过。
- 正式运行使用的 clean worktree 固定在 `6adf49e09ce91b53d2573cb8d34c60dc6a9ec47c`。
- StarryOS kernel、DTB、controller、restart rootfs、Zephyr restart 镜像的 SHA-256 依次为 `590a901a2ed51d9a2d5849ec152576fe822b81f7ab27cfbb2a76a343905ab2fe`、`0f533e1107894dd9b3f062f726fee012519c3e55ef0a2e81e2507e7e3ef303cd`、`fbb1c214b6c771ef415c5768b2e0675ab8e53339f126eef0aa9808fd41987501`、`9e092ad3e0ec4c9842732f8ff0b9475005f1fe2c80cf35d329d9e77b6e7e9ca4`、`400421a6c80862cbd64d9bc6472c77b622dcd5d3f48d51f6262ba1a4d5e13abb`。

## 5. E1：客户机重启恢复正式闭环

### 5.1 先执行一次 clean smoke

```bash
bash competition/ivc/run-orangepi-5-plus.sh fault-restart \
  --repeat 1 \
  --board OrangePi-5-Plus \
  --result-dir /tmp/starry-ivc-restart-smoke \
  --timeout 900 \
  --restore-linux \
  --require-clean
```

smoke 必须验证：

- AxVisor 在实际停止并重建 VM 后输出完整 ARMED/COMPLETE 记录；
- 第二次 StarryOS 启动完成；
- reset 前 20 个和 reset 后 100 个 fresh command 均应用；
- stale STATUS/ACK 被识别，旧 session CONTROL 被拒绝；
- post-reset 控制器显式重发当前 session 的 `seq=1`，并分别验证一个 STATUS 和一个 ACK；
- safe fallback、session reset/rejection、endpoint recovery 均发生一次；
- raw、snapshot、哈希、fsck 和 Linux 恢复全部通过。

若仅因 UART 损坏失败，先修复记录冗余或采集时序；不得降低分析器的完整性门。

clean smoke 执行记录（2026-08-04）：

- [x] 从 `6adf49e09ce91b53d2573cb8d34c60dc6a9ec47c` clean worktree 完成 1/1 实体运行，restart analyzer、raw/snapshot/hash、Linux 恢复与结果镜像 fsck 全部通过。
- [x] 计数严格满足预注册：120 fresh、1 duplicate、122 STATUS、122 ACK、1 ERROR；safe fallback、recovery、stale STATUS/ACK 与 retired CONTROL rejection 均恰好一次。
- [x] 真实重建 VM 1，绑定 host CPU 3；请求与观测 restart delay 均为 20,000 ms。
- 通过档案固定为 `competition/results/orangepi-5-plus/starry-ivc-restart-smoke-pass-20260804/`，仅用于 smoke 验证，不计入正式 3/3。
- `starry-ivc-restart-smoke-restore-config-failure-20260804/` 等此前失败档案保持失败分类；离线 replay 只验证修复，不改变原运行结论。

### 5.2 冻结正式预注册

正式 3 次运行使用以下不可变 capture contract：

| 字段 | 固定值 |
| --- | ---: |
| profile/analyzer | `fault-restart` / `restart` |
| repeat count | 3 |
| pre-reset fresh commands | 20 |
| post-reset fresh commands | 100 |
| expected fresh applications | 120 |
| expected duplicate receives | 1 |
| expected duplicate sequences | `[1]` |
| expected STATUS / ACK | 122 / 122 |
| expected ERROR / protocol error | 1 / 1 |
| session reset / rejection | 1 / 1 |
| safe fallback / endpoint recovery | 1 / 1 |
| stale STATUS / stale ACK | 1 / 1 |
| retired CONTROL rejection | 1 |
| restarted VM / host CPU | VM 1 / pCPU 3 |
| requested restart delay | 20,000 ms |
| ready timeout | 30,000 ms |
| actual VM reset required | `true` |

预注册修订（2026-08-04）：前两次诊断运行中的 `duplicate seq=1` 来自时序相关的自然重传，随后一次 clean-source smoke 在首包 ACK 及时到达时得到 `duplicates=0`，证明旧注入方式不具确定性。自提交 `8f03881d5233f7c95b135f6eba670073b54f55b1` 起，post-reset 控制器显式重发当前 session 的 `seq=1`，验证其 STATUS/ACK 后输出三份带 CRC 的 `IVC-RESTART-D` 记录；两阶段普通发送的 ACK 超时固定为 1000 ms，以避免 100 ms socket 轮询触发偶然重传。最终计数契约仍为恰好一个 duplicate 和 122/122 个 STATUS/ACK，没有放宽验收门。此前失败结果只保留为诊断证据，正式 3/3 只能从该提交或其后经验证的 clean commit 开始。

证据完整性修订（2026-08-04）：CH340 串口可能把同一 SHA-256 截成不同长度片段。分析器只接受彼此呈前缀包含关系的片段，选取最长片段，并要求其与 snapshot/harvest 独立计算出的完整 SHA-256 一致；任意分叉或不兼容片段仍立即失败。该规则由提交 `6adf49e09ce91b53d2573cb8d34c60dc6a9ec47c` 的正反回归覆盖，不降低数据完整性门。

### 5.3 正式运行与聚合

```bash
bash competition/ivc/run-orangepi-5-plus.sh fault-restart \
  --repeat 3 \
  --board OrangePi-5-Plus \
  --result-dir "$RESTART_CAMPAIGN_ROOT" \
  --timeout 900 \
  --restore-linux \
  --require-clean
```

随后使用 `competition/ivc/aggregate_board_campaign.py` 汇总冻结的 preregistration、amendment、最终板卡健康检查和 3 个 run。

正式执行记录（2026-08-04）：

- [x] `capture-001/fault-restart/run-001..003` 均来自 `6adf49e09ce91b53d2573cb8d34c60dc6a9ec47c` clean worktree，采集时间为 00:10:31Z 至 00:22:00Z。
- [x] 三轮均严格得到 120 fresh、1 duplicate、122 STATUS、122 ACK、1 ERROR，以及各一次 session reset/rejection、safe fallback、endpoint recovery、stale STATUS/ACK 和 retired CONTROL rejection。
- [x] 三轮均为 VM 1 在 host CPU 3 上真实 reset；请求/观测 delay 均为 20,000 ms，ready wait 均为 10 ms。
- [x] post-reset 三轮 deadline miss 均为 0，p99 为 8,885/8,402/8,366 µs，worst single-run max 为 25,014 µs。
- [x] pre-reset 三轮各有 1 次 deadline miss，单次 max 为 126,624/119,855/124,313 µs；该不利指标保留在 amendment 与 README 中，不用于放宽 restart 门或声称 RT 改善。
- [x] post-capture 聚合器提交 `7c1ba13af577d82fe89b912bde868604763618f0` 采用 regression-first 支持经独立完整 digest 验证的 UART SHA 前缀，完整 IVC Python 测试 112/112 通过。
- [x] 正式档案为 `competition/results/orangepi-5-plus/starry-ivc-restart-formal-20260804/`；`campaign-summary.json` SHA-256 为 `935db25de96c83267b8d11ea8e55a2909a42a07ca8f2614cef687dce153e2302`。

### 5.4 E1 退出条件

- [x] 3/3 次都通过 restart analyzer。
- [x] 每次均为真实 VM reset，而不是只重启用户态控制器。
- [x] 三次的 fresh/duplicate/stale/ERROR/ACK/STATUS 计数与预注册逐项一致。
- [x] 所有 raw、metadata、summary、console 和 checksum 完整。
- [x] campaign summary 明确给出 restart formal gate 为 true。
- [x] 每次结束都恢复 Linux，最终板卡健康检查通过。

## 6. E2：StarryOS manual/neural 正式实体配对

### 6.1 实验设计

- 固定同一板卡、AxVisor/StarryOS/Zephyr 镜像、vCPU/pCPU 映射、网络、控制周期、初始条件和命令数。
- 唯一实验变量为控制策略：`manual` 与 `neural native`。
- 预先冻结 5 对运行顺序，使用 AB/BA 轮换，避免温度和先后顺序偏差。
- 每个半程均从 clean commit 运行 full profile，不以 smoke 数据代替。
- primary metrics：RMSE、IAE；secondary metrics：最大超调、稳定时间、端到端延迟、deadline miss、协议错误和恢复次数。

### 6.2 验收原则

- [x] manual 5 次、neural native 5 次全部完成并通过 raw/hash/fsck 门。
- [x] 每一对配置除策略外完全相同。
- [x] 报告逐对改善、median、IQR 和 worst-of-runs。
- [x] 已知 RMSE/IAE 改善与最大超调退化同时进入结论。
- [x] 不把 Linux/QEMU manual 结果作为 StarryOS 实体基线。

冻结记录（2026-08-04）：正式活动 `starry-ivc-control-formal-20260804-v5` 来自 clean commit `f4ced37584964aba56e07ff060ae58374608bc26`，按 AB/BA/AB/BA/AB 完成 5 对、10 个有效 half。neural 的 RMSE/IAE 分别改善约 35.93%/51.94%，但最大超调退化约 96.32%；full-loop p99 只有 2/5 配对有利，因此只声明控制误差改善，不声明实时延迟优势。该证据已由提交 `d43dfbbffa9139e53c5e96f502badf00be953cf0` 汇入文档。

## 7. E3：同源 ONNX 与 RK3588 NPU 主路线

### 7.1 固定模型来源，不引入训练

模型来源固定为当前 `thermal-4x6x1-v1` 的 4×6×1 权重：

```text
canonical weights JSON
  -> thermal-4x6x1-v1.onnx
       -> thermal-4x6x1-v1-rk3588-fp16.rknn
       -> thermal-4x6x1-v1.ort
```

- 不建立数据集、训练脚本或调参流程。
- 正式数据开始后不修改权重。
- ONNX 图固定为 `[1,4] -> Gemm -> Relu -> Gemm -> Clip -> [1,1]`。
- native、RKNN 与 ORT 共用输入归一化、权重顺序、输出解释和执行器取整规则。
- 生成 manifest，记录工具版本、转换参数、算子列表、许可证和所有产物 SHA-256。
- 建立 golden vectors 和 10,000 组确定性差分输入。

M4-0/M4-1 已完成并通过以下门：

- canonical weights JSON 是唯一可编辑数学来源；Rust `f32` 常量由其按精确 bit pattern 生成；
- Python 3.10.12、NumPy 1.26.4、ONNX 1.16.1、protobuf 4.25.4 已锁定；
- 两个全新临时目录重建得到字节一致的 Rust、ONNX、golden vectors 和 manifest；
- Rust native oracle 对 10,000/10,000 个输入逐 bit 一致；
- host ONNX Runtime CPU EP 最大绝对误差为 `2.980232238769531e-07`，9999/10000 个执行器命令逐值一致，另 1 个是同一 `0.4745` 半千分位边界两侧的相邻命令，物质性不一致为 0。

### 7.2 RK3588 NPU 平台与所有权审计

审计结论记录在 [`model/rk3588-npu-passthrough-audit.md`](ivc/model/rk3588-npu-passthrough-audit.md)：

- 仓库已有裸机 StarryOS `librknnrt.so -> /dev/dri/card1 -> RKNPU` 路线，但当前比赛 AxVisor guest 配置没有 NPU passthrough，合成 guest DTB 也没有 NPU 节点；两者不能混为同一证据。
- 实体 Linux DTB 已冻结 NPU core MMIO `0xfdab0000`/`0xfdac0000`/`0xfdad0000`（各 `0x10000`）、GIC SPI 110-112、四段 IOMMU MMIO，以及 PMU/CRU 依赖。
- PMU 与 CRU 是全 SoC 共享资源，不整段直通。首个可验证方案由 AxVisor host board glue 完成 power/clock/reset 初始化并冻结时钟，随后把 NPU core MMIO 独占交给 StarryOS guest。
- 初始 spike 使用 polling，不向 guest 暴露 IRQ、IOMMU、OPP 或 regulator；稳定后再分别评审中断和 IOMMU 所有权，禁止 host/guest 同时操作同一 NPU 或共享控制寄存器。
- 在 guest 资源 handoff、真实 submit/device-time 和零 CPU fallback 均有证据前，不得宣称 AxVisor + StarryOS 已完成 NPU 推理。

### 7.3 RKNN NPU 可行性门

RK3588 NPU 不通过 ONNX Runtime CPU EP 使用。本项目的 NPU 主路径固定为：

```text
ONNX -> RKNN Toolkit2 -> .rknn
     -> librknnrt.so
     -> StarryOS RKNPU driver/ioctl ABI
     -> RK3588 NPU
```

按以下顺序执行：

1. [x] 在固定 WSL2 环境中导出 ONNX，并验证二次生成一致性。
2. [ ] 用固定 RKNN Toolkit2 2.3.2 生成 RK3588 FP16 `.rknn`，检查关键算子全部进入 NPU 图并记录 wheel 来源、SHA-256 和许可证边界。
3. [ ] 在板卡 Linux 中完成同模型 reference inference，冻结 Runtime 2.3.2、板端 driver 版本和输出。
4. [x] 审计 StarryOS RKNPU 设备节点、ioctl ABI、mmap、DMA/IOMMU、cache、中断路径和 AxVisor guest 资源缺口。
5. [ ] 实现 host 初始化/guest 独占 handoff，并审计 `librknnrt.so` 的 AArch64 动态依赖以及 StarryOS 所需 syscall；只补齐语义明确、可回归的最小缺口。
6. [ ] 在 StarryOS 完成离线单次、循环 10,000 次和多次加载/卸载测试。
7. [ ] 接入现有 IVC 控制器，完成 StarryOS -> Zephyr 闭环 full profile。

每次 NPU 结果必须记录：

- `backend=rknn-npu`；
- ONNX/RKNN SHA-256；
- RKNN Toolkit、Runtime、RKNPU driver/ABI 版本；
- NPU core mask；
- submit 成功计数、device-time 或等价硬件执行证据；
- 初始化、推理、端到端延迟和 deadline miss；
- 明确的零 CPU fallback 证据。

### 7.4 RKNN go/no-go

只有以下条件全部满足才进入正式 NPU 5 次 full：

- [ ] ONNX 可被固定 RKNN 工具链接受，关键算子无 custom CPU fallback。
- [ ] Linux reference 与 StarryOS NPU 数值满足预注册误差门。
- [ ] StarryOS 能稳定加载同一 `.rknn`，10,000 次离线运行无错误和资源泄漏。
- [ ] 能证明实际 NPU submit/device execution，而非仅初始化 Runtime。
- [ ] 控制周期、rootfs 空间、内存高水位和重复性满足预算。

数值验收不把浮点舍入边界误写成“100% 命令逐值一致”：native/ORT 的浮点误差必须 `<= 1e-6`，RKNN FP16 初始门为 `<= 1e-3`；同时报告执行器 exact-match 数量。非 exact 只允许相差 1 个千分位，且两个输出都位于同一个半千分位取整边界的对应误差窗内；任何更大差值、不同边界、NaN/Inf 或 shape mismatch 都是物质性失败。RKNN 的最终误差窗必须在固定 corpus 上预注册，正式数据开始后不得放宽。

若只能依赖不可再分发组件、ABI 无法兼容、只能在 Linux 工作、实际落入 CPU 或持续错过 deadline，则记录 M4-core no-go 证据。不得把 Runtime 初始化成功称为 NPU 推理成功。

对于 4×6×1 小模型，NPU 提交开销可能大于 native CPU 计算。验收目标是证明硬件卸载和可扩展性，不预设“必然加速”；只有 wall-clock 数据确实改善时才使用“加速”表述。

## 8. E4：ONNX Runtime CPU 对照路线

ONNX Runtime 路线定位为 CPU 对照和标准 Runtime 兼容性增强项，不承担 RK3588 NPU 加速：

1. [ ] 固定 ONNX Runtime release/commit 和 AArch64 musl minimal build 配置。
2. [ ] 从同一 ONNX 生成 `.ort` 与 reduced operator/type config。
3. [x] 在 host CPU EP 完成加载、golden vectors 和 10,000 组差分；最大绝对误差 `2.980232238769531e-07`，物质性命令不一致为 0。
4. [ ] 审计 StarryOS 的线程、futex、mmap、时间、文件和动态链接需求。
5. [ ] 通过可行性门后执行 StarryOS 离线测试和 5 次 full 闭环。

环境拆分为两条可复现路径：核心 ONNX/RKNN 转换继续使用锁定的 Python 3.10.12；ORT host/构建 spike 使用独立 Python 3.12 环境，因为当前 ORT 1.24/1.25 wheel 不再提供 CPython 3.10 构建。最终 ORT 版本、lock 和 AArch64 musl 配置必须在实体可行性门确认后一起冻结，不能把探索环境写成正式工具链。

退出条件：

- [ ] `.ort`、operator config 和 Runtime 构建可由固定环境重建。
- [ ] native/ORT 数值和执行器命令满足预注册门。
- [ ] 实体结果明确记录 `backend=onnxruntime-cpu`，且不存在静默回退。
- [ ] 若 StarryOS ABI 或资源成本不可接受，保留明确 no-go 证据，但不阻塞 RKNN NPU 主路线和现有 native 交付。

本项目不新增自定义 ONNX Runtime RKNPU Execution Provider，也不把 RKNN Runtime 结果标成 ONNX Runtime NPU。

## 9. E5：正式总矩阵与交付

### 9.1 尚需完成的正式矩阵

| 类别 | 配置 | 次数 |
| --- | --- | ---: |
| 控制 | StarryOS neural RKNN NPU full | 5（通过 RKNN 门后） |
| 控制 | StarryOS neural ONNX Runtime CPU full | 5（通过 ORT 门后） |
| 健康检查 | 每个最终 smoke 镜像 | 3 |

M2 的受控干扰 5 对、双 soak、CPU1 stress 5 对，以及 ACK loss、ERROR、restart recovery 的 3/3 结果不重复采集；除非最终代码或镜像变化会影响其结论，届时必须建立新的预注册 campaign，而不是覆盖旧证据。

### 9.2 文档更新

- [ ] `competition/design.md`：StarryOS 三任务架构、重启状态机、ONNX/RKNN/ORT 边界。
- [ ] `competition/test-report.md`：只引用通过正式门的结果，包含不利指标和 no-go 记录。
- [ ] `competition/reproduce.md`：从 clean WSL2 到板卡部署、运行、恢复和回收的单命令流程。
- [ ] `competition/README.md`：给出最短评审入口和结果索引。
- [ ] `competition/video-storyboard.md`：5 分钟内展示自动化、三任务、三种故障、manual/neural 和真实 backend 证据。

### 9.3 最终完成定义

- [x] 任务一、二、三均有 StarryOS 实体板正式结果。
- [x] 受控干扰场景中的 p99 与 worst-case 改善由多次配对 raw data 支撑，并明确不外推到未通过场景。
- [x] manual/neural 为同板同配置 5 对正式实验。
- [x] ACK loss、ERROR、restart recovery 均为实体跨客户机 3/3。
- [ ] ONNX 是唯一跨后端模型来源，所有模型和工具链版本可追溯。
- [ ] RKNN 后端有真实 RK3588 NPU 执行证据；若 no-go，则限制原因和原始证据完整。
- [ ] ORT CPU 后端通过实体门或留下明确 no-go，不冒充 NPU 路线。
- [ ] 每个正式结论均能追溯到 clean commit、配置、镜像哈希、raw、summary 和 checksum。
- [x] WSL2 能自动完成板卡部署、冷启动、采集、恢复 Linux、fsck 和结果同步。
- [ ] 设计、测试、复现和视频内容与冻结的最终数据一致。

## 10. 风险与处理策略

| 风险 | 处理策略 |
| --- | --- |
| CH340 UART 粘连或截断关键记录 | 关键 guest/host 记录错峰重复；SHA 片段仅在互为前缀且匹配独立完整 digest 时归并，任何冲突仍失败 |
| clean worktree 缺少 ignored Zephyr binary | 每个 run worktree 显式重建或复制并记录 SHA-256、来源 commit |
| 板卡写测试造成 ext4 不一致 | 所有写测试以 Linux sync/fsck/boot 检查包围，失败后保留镜像和日志 |
| restart 结果功能成功但证据不完整 | 记为失败运行；修复采集后新建 result set，禁止人工补写 raw marker |
| RKNN Toolkit/Runtime/driver ABI 不兼容 | 先做 Linux reference 与 StarryOS 离线 spike；版本冻结，通过门后才接闭环 |
| vendor 二进制许可证或再分发限制 | manifest 记录来源与许可；不可再分发时提供获取/校验流程，不把文件直接纳入仓库 |
| 小模型 NPU 端到端更慢 | 如实报告为硬件卸载与扩展性验证，不虚构加速收益 |
| ONNX Runtime 引入过大 ABI/镜像成本 | reduced operator build；可行性门失败则形成 no-go，不阻塞 RKNN/native |
| dirty 工作区污染正式结论 | 正式 runner 强制 `--require-clean`，结果 metadata 记录 tracked/untracked count |

## 11. 最近三项动作

1. 提交并冻结 M4-0/M4-1：canonical weights、generated Rust、ONNX、10,000 golden vectors、manifest、锁文件和确定性二次重建门。
2. 完成 M4-2：固定 RKNN Toolkit2 2.3.2 获取/校验流程，转换 RK3588 FP16 `.rknn`，保存完整日志并确认关键算子无 CPU fallback。
3. 使用同一 `.rknn` 先跑 OrangePi Linux 10,000 次 reference；通过数值、device-time 和版本门后，再实现 AxVisor host 初始化与 StarryOS guest 独占 NPU handoff。
