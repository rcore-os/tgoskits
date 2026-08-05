# Competition Execution Plan

> 状态：正式实验已冻结，E5 交付收尾中
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

| 工作包 | 当前状态 | 收尾约束 |
| --- | --- | --- |
| WSL2 板卡自动化 | 已打通；native、RKNN 与 ORT 路线均能从 clean commit 自动完成部署、冷启动、串口采集、独立分析、fsck、Linux 恢复和结果同步 | 保持正式结果只读；录制时复用相同命令或回放未剪辑日志 |
| StarryOS 实体任务一/M2 | 正式受控干扰 5 对、双 soak、CPU1 stress 5 对已完成并汇入报告 | 录制时严格限定改善结论的适用场景 |
| ACK loss | 正式 3/3 已完成并汇入报告 | 不以历史 QEMU 单轮代替实体结果 |
| ERROR | 正式 3/3 已完成并汇入报告 | 展示五类精确 ERROR 与后续正常恢复 |
| restart recovery | `6adf49e09` 实体正式 3/3 已完成，campaign formal gate 为 true，已汇入报告 | 保留 pre-reset deadline miss 不利指标 |
| StarryOS manual/neural full | `f4ced3758` 上正式 5 对、10/10 half 已完成并汇入报告 | 同时披露 neural 最大超调退化和无稳定延迟优势 |
| 模型单一来源与 ONNX | M4-0/M4-1 已完成：固定权重、Rust oracle、10,000 vectors、确定性 ONNX 和 manifest 均已验证 | 保持冻结，不按后续实体结果修改模型 |
| RK3588 NPU | clean resource run 已完成 20 次 context 生命周期；`c3f01dc34` 上的闭环 v8 已完成 5 次冷启动 full、9,000/9,000 ACK、自动 Linux 恢复、独立分析和递归 checksum，已汇入报告 | 同时展示首周期 5 次 miss 与其后 8,995 个零 miss 周期；除非改变启动语义，否则不重跑冻结结果 |
| ONNX Runtime CPU | 离线门与 smoke 已通过；clean commit `0110647de` 的 v4 正式活动完成 5 次冷启动 full、9,000/9,000 ACK、独立重聚合和递归 checksum，formal gate 为 true | 冻结 v4；在最终报告中明确它是 `CPUExecutionProvider` 对照而非 NPU |
| 报告与视频 | 正式数据已冻结，五份交付文档和入口 README 已同步收口；实际视频尚未录制 | 录制约 5 分钟演示并在获授权后准备目标分支 PR |

当前执行顺序固定为：

```text
E0 工作区与证据冻结（完成）
  -> E1 restart 证据加固与正式 3/3（完成）
  -> E2 StarryOS manual/neural 正式 5 对（完成）
  -> E3 同源 ONNX 与 RKNN NPU 主路线（完成）
  -> E4 ONNX Runtime CPU 对照路线（完成）
  -> E5 总体验收、报告和视频（文档完成；视频/PR 待完成）
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

### 4.1 执行清单

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

M4-2 已完成并通过以下门：

- Python 3.10.12 和全依赖 hash lock 固定 Toolkit2 2.3.2；官方 wheel 来自 commit `42aa1d426c0a9e0869b6374edba009f7208a1926`，SHA-256 为 `6cb783ddf293ac509f39bf9127acf6a5492bbb67e4b4b4ac33a7c6d2cefb4f3c`，因上游 LICENSE 没有肯定的再分发授权而不提交 vendor wheel；
- 冻结 `.rknn` 为 15,873 bytes，SHA-256 `2ad3fecedc9767ee57cbcd31787f70297a8f8e2cfcdc8e07b81b949566d53bb8`；通过 child 启动前的 `MALLOC_PERTURB_=255`/`PYTHONHASHSEED=0` 消除 Toolkit2 固定内部区间的进程内存相关字节，两个独立重建逐字节一致，不做 binary patch；
- 两个模型计算节点被编译为 NPU `ConvRelu` 和 `ConvClip`，无 custom CPU op；CPU 只承担输入/输出 wrapper 与最终 reshape；
- 同一 ONNX/同一 RK3588 FP16 config 的 Toolkit host simulator 对 10,000 vectors 相对 native f32 最大误差为 `0.001798778772354126`，命令差直方图为 `-2:23, -1:322, 0:9369, +1:280, +2:6`；相对确定性 FP16 oracle 最大误差为 `0.00048828125`、逐值一致 9806/10000；
- Toolkit2 host simulator 不支持执行 `load_rknn()` artifact，因此该差分不是硬件证据；Linux reference 必须执行提交的同一 `.rknn`。

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
2. [x] 用固定 RKNN Toolkit2 2.3.2 生成确定性 RK3588 FP16 `.rknn`，检查两个模型计算节点进入 NPU 图，完成 10,000-vector host simulator 差分并记录 wheel 来源、SHA-256 和许可证边界。
3. [x] 在板卡 Linux 中完成同模型 reference inference，冻结 Runtime 2.3.2、板端 driver 版本和输出。
4. [x] 审计 StarryOS RKNPU 设备节点、ioctl ABI、mmap、DMA/IOMMU、cache、中断路径和 AxVisor guest 资源缺口。
5. [x] 实现 host 初始化/guest 独占 handoff，并审计 `librknnrt.so` 的 AArch64 动态依赖；host 只负责 power/clock/reset，交接后 `host_submit=false`。
6. [x] 在 StarryOS 完成一次冷启动下的离线 10,000 次实体 NPU spike，并由 raw data、正 device-time、Runtime/driver 和快照内产物哈希交叉验证。
7. [x] 从 clean commit 完成至少 3 次冷启动、多次加载/卸载、内存高水位和 rootfs 空间验证。
8. [x] 接入现有 IVC 控制器，完成 StarryOS -> Zephyr 闭环 full profile。

每次 NPU 结果必须记录：

- `backend=rknn-npu`；
- ONNX/RKNN SHA-256；
- RKNN Toolkit、Runtime、RKNPU driver/ABI 版本；
- NPU core mask；
- submit 成功计数、device-time 或等价硬件执行证据；
- 初始化、推理、端到端延迟和 deadline miss；
- 明确的零 CPU fallback 证据。

M4-3 物理 spike 记录（2026-08-04）：

- AxVisor 只初始化 RK3588 NPU 的 3 个 core、3 个 power domain、8 个 clock 和 6 个 reset，随后把三个 NPU core MMIO 与 `[0x80000000,0x90000000)` identity DMA 区间交给 StarryOS；guest 使用 polling，不暴露 IRQ、IOMMU 或共享 PMU/CRU，串口明确记录 `host_submit=false`。
- `rknpu-spike-20260804-v3` 在一次冷启动中完成 32 次 warm-up 和 10,000/10,000 次推理，Runtime API 为 `2.3.2`、StarryOS driver 为 `0.9.8`、core mask 为 0，10,000 个 device-time 全部为正，零 run/perf-query error；Linux 随后自动恢复为 `/dev/mmcblk1p2 ext4 rw`。
- `rknpu-spike-20260804-v8-split-evidence` 首次由集成 runner 在一次命令中直接返回 PASS：pre-run source capture、实体推理、96 MiB 快照、Linux 恢复、独立数值分析和递归 checksum 全部闭环。该轮 raw SHA-256 为 `bb6bd18aa60fed170f328ec2701aa3b198d02d0f907bc50a1aba4a1f10f79853`，runner SHA-256 为 `b81fede54c63f6792905603ba7b515e36a151f71e2263a6291af00f5cc968703`，快照 SHA-256 为 `11fe30eab32d997123ec0098ca8488b27063bfef5266c75dee4907197c4cb83a`；模型、corpus 和 Runtime 仍匹配冻结哈希。
- v8 的 device p50/p95/p99/max 为 `1565/1646/1680/2227 us`，wall p50/p95/p99/max 为 `1645875/1729000/1763125/2308833 ns`，初始化为 `78742 us`。4 组完整 compact runtime/result、5 条 PASS、4 条 raw 绑定、5 条 snapshot sync 和 4 条 host sync 被分析器接受；1 个非 UTF-8 串口字节仅记录为 replacement，不影响原始 console 哈希或 ASCII 凭据匹配。
- 相对 native f32 的最大绝对误差为 `0.001798778772354126`，执行器命令差为 `-2:23, -1:322, 0:9369, +1:280, +2:6`；相对 FP16 oracle 的最大误差为 `0.00048828125`、逐值一致 9806/10000，均与冻结的 simulator/Linux reference 一致。
- StarryOS device p50/p95/p99/max 为 `1565/1646/1676/2205 us`，wall p50/p95/p99/max 为 `1646458/1729291/1759333/2287250 ns`，初始化为 `78179 us`。该小模型在 StarryOS 虚拟化路径上明显慢于 Linux reference，因此只声明真实硬件卸载与控制周期预算通过，不声明 NPU 加速。
- 分析器从 96 MiB 快照提取并逐哈希核对 runner、`.rknn`、corpus 和 `librknnrt.so`；完整哈希分别为 `f74891c7eb599c2a5c6f3bc758f9f3fe0efe3f96734ab5bd3f66510588ce7e6d`、`2ad3fecedc9767ee57cbcd31787f70297a8f8e2cfcdc8e07b81b949566d53bb8`、`907d3a9b93bcf8fc15ec11465025eb10a9219a0162d2cf060aadc4e9eab63310` 和 `d31fc19c85b85f6091b2bd0f6af9d962d5264a4e410bfb536402ec92bac738e8`。raw SHA-256 为 `1b6502ab4cf8230057c0ab216f4bd980186ff45511069238a5d71b702e7b47f3`，快照 SHA-256 为 `ddedb12c02d1d8a893ed8e1fc6ab001a4109a918b8a0c1fc5a68d1764c826198`。
- 原始 v3 证据保持在 `tmp/competition/ivc/rknpu-spike-20260804-v3/` 且原 checksum 已复核；事后独立分析位于并列的 `rknpu-spike-20260804-v3-analysis/`，不会改写原目录。报告明确标记 `source.provenance=reconstructed-post-run`、`source.dirty=true`，因此不得计入 formal 运行。
- v4-v7 失败目录均原样保留并通过各自 checksum：v4 暴露单份 runtime/result 长行截断，v5 暴露 boot log 非 UTF-8 字节，v6 暴露三份超长 PASS 均损坏，v7 暴露三份超长 result 均损坏。修复依次采用失败证据封存、二进制串口 replacement 解码、PASS/raw 分离及 runtime/result 分段短凭据；不得把任何失败目录补写或重标为成功。
- v1/v2 失败目录继续保留：v1 暴露 CH340 host sync 标记损坏，v2 暴露 ostool 2048-byte 流匹配窗口无法跨越整段日志；修复采用 5 份 100 ms 间隔的最终同步标记和只匹配终态行的有界正则。集成后的 runner 会在运行前记录来源、提取快照内四个部署产物、运行独立分析器并递归生成 checksum；formal 模式同时要求 clean tree 和 pre-run source capture。

M4-4 clean resource 与正式闭环记录（2026-08-04 至 2026-08-05）：

- `rknpu-formal-696bc2f-resource-01-20260804` 来自 clean commit `696bc2f467beb073b21f7e729623445f6a61a684`，完成 20 次 context init/destroy 和 19 次 probe inference；第一次销毁后的 RSS 为 5,164 KiB，最终为 5,172 KiB，增长 8 KiB，rootfs 剩余 51.13%。10,000-vector 分析、Runtime/driver、raw、嵌入产物和递归 checksum 均通过。
- 闭环正式活动 [`rknpu-control-full-formal-20260805-v8`](results/orangepi-5-plus/rknpu-control-full-formal-20260805-v8/) 的 `rknpu-full/run-001..005` 全部来自 clean commit `c3f01dc34b83695eddf8da83cf4ed71622f64f7c`，每轮 1,800 个样本，共 9,000/9,000 ACK，ERROR、timeout、retransmission、recovery 均为 0；Runtime API 为 2.3.2、driver 为 0.9.8，模型 SHA-256 保持 `2ad3fecedc9767ee57cbcd31787f70297a8f8e2cfcdc8e07b81b949566d53bb8`。
- 五轮 full-loop p99 为 13,502/13,492/13,611/13,456/13,520 us，NPU device p99 为 1,670/1,669/1,666/1,678/1,678 us。每轮唯一 deadline miss 都是 `sequence=1`：全样本仍保留 5/9,000 miss，首周期 worst 为 145,522 us；其后 8,995 个周期为 0 miss，跨五轮 worst 为 17,884 us。该分层只解释 cold-start 成本，不从总指标中删除失败周期。
- 聚合器提交 `1d2af15c2` 验证每轮固定 9 文件 manifest、clean source、输入/输出身份、UART 双份哈希、raw/RKNN 数值和版本；提交 `398932fef` 进一步从 raw CSV 重算全样本、首周期和后续周期。不可覆盖的 [v2 聚合文件](results/orangepi-5-plus/rknpu-control-full-formal-20260805-v8/rknpu-control-full-formal-20260805-v8-aggregate-v2-398932fef.json) SHA-256 为 `dfc7d844b4d219992d72e7b8be22a18be6b49d4e18feca993df2eaad2eff6f27`。
- commit `2998356c5` 将五轮 console/raw/RKNN CSV、metadata、summary、stage log、per-run manifest 和两版 aggregate 原样归档；归档不包含 rootfs、Toolkit wheel、Runtime library 或其他 vendor binary。迁移后独立重聚合除 `campaign.path` 自定位字段外与冻结 v2 aggregate 完全一致。

### 7.4 RKNN go/no-go

只有以下条件全部满足才进入正式 NPU 5 次 full：

- [x] ONNX 可被固定 RKNN 工具链接受，两个模型计算节点无 custom CPU fallback。
- [x] Linux reference 与 StarryOS NPU 的单次物理 spike 数值满足预注册误差门。
- [x] StarryOS 已有单命令闭环的 10,000 次离线无 Runtime/run/perf-query error 结果；仍需 clean 冷启动重复、加载/卸载和增长型资源泄漏证据后才能称为稳定。
- [x] 10,000 个正 `RKNN_QUERY_PERF_RUN` device-time、guest RKNPU 注册和 `host_submit=false` 共同证明实际 NPU execution，而非仅初始化 Runtime。
- [x] 控制周期、rootfs 空间、内存高水位和重复性满足预算；cold-start 首周期 miss 单独披露且仍计入总数。

数值验收不把浮点舍入边界误写成“100% 命令逐值一致”。native/ORT 的浮点误差必须 `<= 1e-6`，仅接受同一半千分位边界 `1e-6` 窗内相差 1 的命令。RKNN FP16 使用在固定 corpus 上、实体实验前冻结的独立门：相对 native f32 最大绝对误差 `<= 0.002`，执行器命令差绝对值 `<= 2` 并报告完整直方图；相对确定性 FP16 oracle 最大绝对误差 `<= 0.0005`。任何更大差值、NaN/Inf、backend error 或 shape mismatch 都是物质性失败，Linux/StarryOS 实体结果不得触发门限放宽。

若只能依赖不可再分发组件、ABI 无法兼容、只能在 Linux 工作、实际落入 CPU 或持续错过 deadline，则记录 M4-core no-go 证据。不得把 Runtime 初始化成功称为 NPU 推理成功。

对于 4×6×1 小模型，NPU 提交开销可能大于 native CPU 计算。验收目标是证明硬件卸载和可扩展性，不预设“必然加速”；只有 wall-clock 数据确实改善时才使用“加速”表述。

## 8. E4：ONNX Runtime CPU 对照路线

ONNX Runtime 路线定位为 CPU 对照和标准 Runtime 兼容性增强项，不承担 RK3588 NPU 加速：

1. [x] 固定 ONNX Runtime 1.25.0 release、commit `7a71bc575b189cdedea7fa2c0f87389f870bd10e` 和官方 Linux AArch64 glibc archive；archive SHA-256 为 `849c04634e76446bbe0a92f67955a9641415c37f11930804066057bf9eadbd03`。
2. [x] 从同一 ONNX 生成 4,144-byte `.ort` 与 reduced operator/type config；canonical `.ort` SHA-256 为 `3582869baf9b8cec722208d06f66acd680a64128b52875d22e7f0e43f2ed7887`，算子仅为 `Clip`、`Gemm` 和优化后的 `FusedGemm`。
3. [x] 使用冻结 `.ort` 在 host CPU EP 完成 10,000 组差分；最大绝对误差 `2.980232238769531e-07`，9,999 个执行器命令逐值一致，1 个为允许的半千分位边界等价，物质性命令不一致为 0。
4. [x] 完成静态动态链接审计：`libonnxruntime.so.1.25.0` 为 19,215,360 bytes，最高要求 GLIBC 2.27、GLIBCXX 3.4.21、CXXABI 1.3.11；依赖和许可证哈希冻结在 `onnxruntime-1.25.0-source.json`。
5. [x] 用 exact C API runner 在 StarryOS glibc rootfs 完成 `.ort` 加载、10,000-vector 离线差分、线程/futex/mmap/时间/文件 ABI 和内存/空间门。
6. [x] 把 ORT CPU EP 接入现有 StarryOS IVC controller，并在实体 StarryOS→Zephyr smoke 完成 20/20 ACK、逐样本 actuator 交叉核对、快照回收和 Linux 恢复。
7. [x] 按下述预注册门从同一 clean commit 执行 5 次 full 闭环；失败 run 原样保留，最终 v4 从新目录完成全部五轮且不修改离线门或 NPU 正式结果。

环境拆分为两条可复现路径：核心 ONNX/RKNN 转换继续使用锁定的 Python 3.10.12；ORT 导出和 host 验证使用独立 CPython 3.12.11、ONNX Runtime 1.25.0 和 hash lock。目标端先使用官方 AArch64 glibc 完整 Runtime，而不是尚无证据的 musl/minimal build；只有完整 Runtime 实体门通过且镜像成本确有必要时，才增加 reduced-operator minimal build 作为后续优化。

ORT 1.25.0 对该模型的重复导出已观察到两个语义等价但字节不同的 FlatBuffer layout。正式策略不虚构上游 byte determinism：冻结上述 canonical bytes；重建检查只接受两个已审计哈希，并对每次新产物重跑 normalized operator contract 和全部 10,000-vector 输出指纹。任何第三种哈希、算子变化或数值变化都立即失败。

E4 实体离线记录（2026-08-05）：

- `ort-offline-formal-20260805-v1` 使用了 Linux 设备名 `/dev/mmcblk1p2` 作为 AxVisor root selector，AxVisor 因而选择其 `disk1p2` eMMC `misc` 分区并在 guest 启动前失败。该失败目录保持原样，不重命名为通过结果；正确映射是 AxVisor `/dev/mmcblk0p2` 对应恢复后 Linux 的 `/dev/mmcblk1p2`。
- `ort-offline-formal-20260805-v2` 来自 clean commit `2df7da841f5fe778c02bb91aafae9ac908f595d5`。官方 ONNX Runtime 1.25.0 `CPUExecutionProvider` 在 StarryOS 完成 10,000/10,000 次推理；最大绝对误差为 `2.980232238769531e-07`，精确命令 9,999、预注册舍入边界等价 1、物质性不一致 0，输出指纹与 host gate 相同。
- wall p50/p95/p99/max 为 `121333/128042/157208/3090792 ns`，session 初始化为 `1780 us`。5 次 session create/destroy 后主 session 销毁 RSS 相对首次销毁增长 `224 KiB`，peak RSS `16196 KiB`；160 MiB rootfs 剩余 `63.69%`，均通过预注册资源门。
- raw、resource、runner、`.ort`、corpus、`libonnxruntime.so.1`、provider shared 和 160 MiB snapshot 均由 guest manifest、板端 SHA 和独立 analyzer 交叉验证；raw/resource/snapshot SHA-256 分别为 `e4a6b601804377772a707c4e0684b9822e2a916aebe6ed096cdae41910d21ad2`、`fc50de37eeae39c274622f857afc3d4afe267e785d6bf0cf09ffa8311ab3ea19`、`620b278d19e573df9d0b67d0d77000690ed45ee60dc54b6408a7f7d12502d0c9`。递归 `checksums.sha256` 自身 SHA-256 为 `33eac20d68ba9dfc134b8208f924b583cc6c76f595b90b392ad91ac7620a1999`；只读 `e2fsck -fn` 通过，Linux 恢复为 `/dev/mmcblk1p2 ext4`。

E4 实体闭环 smoke 记录（2026-08-05）：

- `ort-control-smoke-formal-20260805-v1` 至 `v3` 均在进入 workload 前被 WSL `/home` 历史执行位损坏阻断；目录保持原样。`v4` 首次进入 StarryOS，暴露 C++ ORT metadata 借用视图早于 `TypeInfo` owner 销毁的生命周期缺陷，并以 `IVC-STARRY-DONE exit=1` 安全失败；该结果同样保留，不计为通过。
- 生命周期回归测试先在缺陷实现上稳定失败，再由 commit `af41cf0861f1381eb99c9f66d13b59519632ebe8` 修复。`v5` 从该 clean commit 完成 20/20 ACK，errors/timeouts/retransmissions/recoveries/protocol errors 均为 0；ORT 1.25.0 明确记录 `CPUExecutionProvider`，模型 SHA-256 为 `3582869baf9b8cec722208d06f66acd680a64128b52875d22e7f0e43f2ed7887`。
- smoke 保留 1 个 sequence 1 cold-start deadline miss：full-loop p99/max 为 `13272/125250 us`；其后 19 个周期零 miss。ORT 初始化为 `218433 us`，inference wall p50/p99/max 为 `144666/168584/17156417 ns`。
- raw/ORT/snapshot SHA-256 分别为 `3c9567750c3d31047b3fad97f4a12fd1e5970246f82eb2b9bb8bf89160d68583`、`c656e3f8f8beaca091df6dd522a9e3880a72382a0aa09950e13a7ddff0505597`、`e5ca46501887f8df5ad48f792819da97cf823b8194577323807807ce6d6ec13b`；递归 evidence manifest SHA-256 为 `356ee9497d1504e0ab1eba97e1b6f9271fa83b48249034492b7812e3acf55655`。独立重跑 analyzer 与冻结 summary 逐字节一致，Linux postcheck 为 `/dev/mmcblk1p2 ext4`。

ORT full 预注册门（在 smoke `v5` 之后、首次 full `run-001` 之前冻结）：

- 固定 5 次冷启动、每次 1,800 个 100 ms 周期，共 9,000 个样本；同一实体板、同一 clean commit、同一 kernel/DTB/rootfs/Zephyr/model/runtime 哈希，执行顺序为 `run-001` 至 `run-005`，不得用补跑覆盖失败 run。
- 每轮必须 1,800/1,800 ACK，errors、timeouts、retransmissions、recoveries、duplicates、acks dropped 和 protocol errors 全为 0；raw 与 ORT CSV 均连续 1,800 行并逐行 actuator 一致。
- 每轮最多允许 1 个 deadline miss，且若存在只能是 sequence 1；sequence 2..1800 必须零 miss。整体 full-loop p99 `<= 25000 us`、包含首周期的 max `<= 200000 us`、throughput `>= 9.5 msg/s`；首周期仍计入总指标和 9,000 样本总数。
- 每轮 ORT 初始化 `> 0` 且 `<= 500000 us`；inference wall p99 `<= 1000000 ns`、max `<= 25000000 ns`；Runtime 必须精确为 `1.25.0`、provider 必须精确为 `CPUExecutionProvider`，禁止静默 fallback。
- 每轮必须通过 UART、guest manifest、harvest 和压缩前内容的完整 SHA-256 一致性，snapshot `e2fsck -fn`、host filesystem sync、Linux 恢复、clean metadata 和递归 checksum 全部为真。任一门失败即 campaign 不通过，门限不得按 full 结果放宽。

ORT full 预注册修订记录（2026-08-05，首次失败后、下一次 campaign 前冻结）：

- `ort-control-full-formal-20260805-v1/run-001` 来自 clean commit `d99ed9932f31c1d417f9edb4148514a815b91353`。功能路径完成 1,800/1,800 ACK，controller/Zephyr 的 errors、timeouts、retransmissions、recoveries、duplicates、ACK drops 与 protocol errors 均为 0；full-loop p99/max 为 `12038/124559 us`，ORT inference wall p99 为 `175875 ns`。raw 与 ORT CSV SHA-256 分别为 `b5b663951fbaa29f2b4860b75131e52a45293c2a937f301ad4dd54cf7dcc648e` 与 `28270563d5d7fdec7047e5fbe1817edd373d6a43389f6e922e9840feb587b4a8`。
- 该轮仍被正式门拒绝：Zephyr 的两份 `IVC-RTOS-POWEROFF` 均与 StarryOS 同时输出的终止指标发生 UART 字节级交错，分析器因此找不到一份完整 poweroff record。控制、推理、快照回收和 Linux 恢复实际完成不改变该证据失败结论；`v1` 目录保持原样，不计为通过，也不以补跑替换。
- commit `df785eead23cba61f49e9bdf49bb4355c8998846` 只修改终止证据节奏：等待 controller 终止输出排空 `500 ms`，再以 `100 ms` 间隔输出 5 份短 poweroff record。分析器、验收阈值、ONNX/ORT 模型、controller 控制逻辑、1,800 个周期及五次冷启动计划均不变。
- 修订后 full Zephyr `zephyr.bin` SHA-256 为 `d02b6de2677c8f2a26db1514ae7d6e0a1723ada329c59bc17b6f58333bd075ff`，`zephyr.elf` SHA-256 为 `5acb3b59e935ff5058c37d9e719486616fa48843a9e4755e6ac736a0a1e155f1`，ELF entry 为 `0x4000100c`。下一次正式 campaign 必须使用新的结果目录和包含本修订记录的 clean commit。

ORT full 第二次预注册修订记录（2026-08-05，`v2` 停止后、`v3` 前冻结）：

- `ort-control-full-formal-20260805-v2/run-001` 来自 clean commit `5f4a7ea9d2759a2956fbf6af2b8db8b185283a8d`。五份 poweroff record 全部完整，证明前述 UART 修订生效；功能路径仍为 1,800/1,800 ACK、零协议/可靠性错误，仅 sequence 1 有一次 deadline miss。full-loop p99/max 为 `12017/146458 us`，throughput 为 `9.991877 msg/s`，ORT wall p99/max 为 `175583/17119958 ns`。
- 当时的 analyzer 在第 1,280 行拒绝证据：`output_bits=3ea72b02` 解码为 f32 `0.3264999985694885`，但 actuator 为 `327`。Rust controller 的实际语义是在每一步执行 f32 舍入，`output * 1000.0F` 得到 `326.5F`，再加 `0.5F` 后截断为 `327`；旧 Python analyzer 将值提升为 f64 后得到 `326`，因此产生假阴性。`v2` 在 `run-001` 后停止，目录保持原样，不追认成正式 campaign，也不补跑其余四轮。
- commit `f3302d1adbf8244045fea7665414ae8aa97666c5` 用逐操作 IEEE-754 f32 舍入复现 controller 换算。来自实体第 1,280 行的确定性回归在旧实现上失败、修复后通过，并同时验证错误值 `326` 仍会被拒绝；完整 Python 回归为 209/209。
- 修复后的 analyzer 对未改动的 `v2/run-001` 原始证据独立复核通过，1,800 个 ORT actuator 与 controller raw CSV 全部一致。该修订不改变模型、Runtime、controller、Zephyr、镜像、采样计划或任何验收阈值；`v3` 必须使用新的结果目录和包含本记录的 clean commit 重新执行完整五轮。

ORT full 第三次预注册修订记录（2026-08-05，`v3` 停止后、`v4` 前冻结）：

- `ort-control-full-formal-20260805-v3` 来自 clean commit `055585db67d577d67ecab41b70a21c3f56068161`。`run-001` 至 `run-003` 的单轮 metadata 均为 validated；`run-004` 完成 1,800/1,800 ACK、快照和 Linux 恢复后被 analyzer 拒绝，因此 `v3` 整体失败，未执行 `run-005`，现有四轮不作为新 campaign 的前四轮复用。
- 第四轮两份短 `IVC-CONTROLLER-RELIABILITY` 均完整且一致，但单份 legacy 长 `IVC-CONTROLLER-RESULT` 在 UART 交错后形成 `success_percent=e_send_p50_us=201`。旧 analyzer 只检查字段名存在，误把这个非浮点值视为“完整”并与短记录判冲突。raw/ORT SHA-256 分别为 `cdd4f34e35c05770d06eccb98d53cea6ba4ef4b219cb1433b6c2b8db4dc987af` 与 `6ca97e741f81bc43b159d07348827fe3e1d5bdae1c8677e28963bea40809d2bf`。
- commit `c05f651cb1617a134a06b732c0ce0460ab04b416` 只让 legacy 字段通过其声明类型后才可参与回退或冲突比较。来自第四轮的确定性回归在旧实现上失败、修复后通过；原有“类型正确但数值冲突的 legacy 必须拒绝”测试继续通过，完整 Python 回归为 210/210。
- 修复后的 analyzer 对未改动的 `v3/run-004` 独立复核通过：仅 sequence 1 有一次 deadline miss，full-loop p99/max 为 `12278/124817 us`，throughput 为 `9.993105 msg/s`，ORT wall p99/max 为 `175584/17172167 ns`，1,800 个 actuator 全部一致。该修订不改变 producer、模型、Runtime、镜像、采样计划或验收阈值；`v4` 必须从包含本记录的 clean commit 和新目录重新执行全部五轮。

E4 正式 full 记录（2026-08-05）：

- [`ort-control-full-formal-20260805-v4`](results/orangepi-5-plus/ort-control-full-formal-20260805-v4/) 从 clean commit `0110647de52f5e2ad6b550cb594780d7506ffecf` 在同一实体板 `bf61f4d4a1d994ad` 完成五次全新冷启动。五轮均为 1,800/1,800 ACK，共 9,000/9,000；errors、timeouts、retransmissions、recoveries、duplicates、ACK drops 和 protocol errors 全为 0，raw/ORT CSV 每轮均连续 1,800 行且 actuator 逐行一致。
- 五轮各保留一个 `sequence=1` cold-start deadline miss，总计 5/9,000；`sequence=2..1800` 为 0/8,995。全样本 full-loop p99 范围为 `12023..12273 us`、max 范围为 `119778..143429 us`、throughput 范围为 `9.9920149415..9.9934469874 msg/s`；去掉首周期后 p99 为 `12021..12266 us`、跨轮 worst 为 `17546 us`。首周期仍保留在正式总指标中。
- ORT 初始化范围为 `218201..224251 us`，inference wall p99 为 `174417..175583 ns`、max 为 `17128709..17160791 ns`。五轮均精确记录 ONNX Runtime `1.25.0`、`CPUExecutionProvider` 和模型 SHA-256 `3582869baf9b8cec722208d06f66acd680a64128b52875d22e7f0e43f2ed7887`。
- 预注册、聚合 summary 与递归 campaign checksum 文件的 SHA-256 分别为 `04768defc09ce5e9a0069ead59bd01ea9fc696b32f46fdcd3619797327beded4`、`57edb5f8a1fc79bcbd43fb3fd77aec25151e7d773985a56b12e6d3530d14d3f9` 和 `601b435f376841dcfbb54e0c8bbac5fd9e6ffb09e4c08c4f67e73f2934d85a25`。在记录的原始正式路径上独立重跑聚合器得到与冻结 summary 字节一致的输出；迁入仓库后重聚合只改变 `campaign.path` 和 `preregistration.path` 两个自定位字段，忽略这两个字段后的结构化结果完全一致。每轮 manifest、campaign checksum、snapshot fsck 和 Linux `/dev/mmcblk1p2 ext4 rw` 恢复均再次验证。
- commit `7c9da6e5a` 将 ORT v1-v4 的原始 console、raw/ORT CSV、metadata、summary、manifests、聚合文件和 host logs 一并归档；未修改任何冻结结果内容。
- `v1` 至 `v3` 仍作为不可覆盖的失败活动保留在相邻结果目录；它们不计入 v4 的五轮，也没有用补跑替换失败 run。

退出条件：

- [x] `.ort`、operator config、host 工具链 lock 和官方 AArch64 Runtime 来源/哈希均已冻结并可复核。
- [x] native/ORT 数值和执行器命令满足预注册门。
- [x] 实体结果明确记录 `backend=onnxruntime-cpu` 和 `provider=CPUExecutionProvider`，且不存在静默回退。
- [x] 五次 clean full 均通过单轮门和 campaign formal gate；全样本与 post-first 指标均从 raw 重新计算并通过独立复核。
- [x] StarryOS ABI 与当前资源门可接受，因此不触发 no-go；仍不阻塞或改写 RKNN NPU 主路线和现有 native 交付。

本项目不新增自定义 ONNX Runtime RKNPU Execution Provider，也不把 RKNN Runtime 结果标成 ONNX Runtime NPU。

## 9. E5：正式总矩阵与交付

### 9.1 正式矩阵状态

| 类别 | 配置 | 次数 |
| --- | --- | ---: |
| 控制 | StarryOS neural RKNN NPU full | 5（已完成，clean v8） |
| 控制 | StarryOS neural ONNX Runtime CPU full | 5（已完成，clean v4，formal gate=true） |
| 生命周期 | RKNN/ORT 每轮冷启动、snapshot fsck、Linux 恢复 | 10/10（随两组 formal full 完成） |

M2 的受控干扰 5 对、双 soak、CPU1 stress 5 对，以及 ACK loss、ERROR、restart recovery 的 3/3 结果不重复采集；除非最终代码或镜像变化会影响其结论，届时必须建立新的预注册 campaign，而不是覆盖旧证据。

### 9.2 文档更新

- [x] `competition/design.md`：StarryOS 三任务架构、重启状态机、ONNX/RKNN/ORT 边界。
- [x] `competition/test-report.md`：只引用通过正式门的结果，包含不利指标和 no-go 记录。
- [x] `competition/reproduce.md`：从 clean WSL2 到板卡部署、运行、恢复和回收的单命令流程。
- [x] `competition/README.md`：给出最短评审入口和结果索引。
- [x] `competition/video-storyboard.md`：5 分钟内展示自动化、三任务、三种故障、manual/neural 和真实 backend 证据。

### 9.3 最终完成定义

- [x] 任务一、二、三均有 StarryOS 实体板正式结果。
- [x] 受控干扰场景中的 p99 与 worst-case 改善由多次配对 raw data 支撑，并明确不外推到未通过场景。
- [x] manual/neural 为同板同配置 5 对正式实验。
- [x] ACK loss、ERROR、restart recovery 均为实体跨客户机 3/3。
- [x] ONNX 是唯一跨后端模型来源；native、ONNX、RKNN 与 ORT artifact/工具链均可追溯，ORT 实体 ABI 门已在 E4 独立通过。
- [x] RKNN 后端已有 clean resource run、真实 RK3588 NPU 10,000-vector 证据和 5 次 clean StarryOS -> Zephyr formal full；dirty spike 只保留为早期可行性记录。
- [x] ORT CPU 后端已通过实体门，明确记录为 CPUExecutionProvider，不冒充 NPU 路线。
- [x] 每个正式结论均能追溯到 clean commit、配置、镜像哈希、raw、summary 和 checksum。
- [x] WSL2 能自动完成板卡部署、冷启动、采集、恢复 Linux、fsck 和结果同步。
- [x] 设计、测试、复现和视频分镜内容与冻结的最终数据一致；实际视频文件仍待录制。

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

1. 按 `video-storyboard.md` 录制约 5 分钟演示，画面同时保留 clean source、backend 身份、formal summary/checksum 和 Linux 恢复证据。
2. 对最终交付 commit 再执行链接、JSON、递归 checksum 与 clean-worktree 审核；冻结的 RKNN/ORT campaign 不因文档或录制重跑。
3. 获得提交权限后检查目标 `dev` 无冲突，推送正式分支并按项目格式提交 PR；PR 不宣称尚未生成的视频文件。
