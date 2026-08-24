# AxVisor StarryOS RT 实体板正式活动

状态：OrangePi-5-Plus 当前源码正式测量基线

## 问题与成功标准

历史 RT 结果具备五组 shared/partitioned 配对与双侧长稳，但执行依赖一次性
`run-half.sh`、硬编码目录和人工顺序控制。它不能自动证明运行期间没有换 commit、
镜像、配置或实体板，也不能阻止失败尝试被误当成正式 half。当前源码 smoke 又只有
一对，单对比较器按设计拒绝打开 M2 出口。因此需要一个可恢复、fail-closed 的正式
活动入口，而不是继续扩写手工操作说明。

直接用户是复现竞赛 RT 数据的维护者和评委。完成标准是：

- 第一块板启动前冻结完整 Git commit/tree、关键源码、输入制品、阈值和 AB/BA 顺序；
- 在同一 OrangePi-5-Plus 上完成五组正交配对和 shared/partitioned 双长稳；
- 每个 half 独立完成 staging、板卡运行、snapshot、harvest、Linux 恢复与证据收据；
- shared host-noise 固定 pCPU1，partitioned 固定 pCPU3，两个 vCPU 固定 pCPU1/pCPU2
  且迁移数为零；
- host/guest 直接 IRQ trace 无丢失，长稳 host-noise 覆盖不少于 1,800 秒；
- 聚合器在冻结阈值下给出 `assessment.m2_exit_gate_met=true`；
- 任一前置条件或证据不成立时拒绝生成正式收据，不伪造成功。

非目标包括改变 AxVisor 调度、timer/GIC、设备图或 StarryOS ABI；这些运行时语义由
现有实现和配置负责。本工具只编排、冻结和验证测量，不建立第二份设备资源状态。

## 内部 prior art 与方案选择

检查的既有能力包括：

- `competition/ivc/run-control-campaign.sh formal` 的预注册顺序与 fail-closed 思路；
- `stage-starry-board.sh`、OrangePi `board-runner.sh`、
  `harvest-starry-board.sh` 的 Linux staging、冷启动、snapshot 和恢复边界；
- `compare_starry_board.py` 与 `aggregate_starry_board.py` 的单对和 M2 聚合语义；
- 2026-08-03 历史正式活动的一次性 `run-half.sh` 和结果目录。

| 方案 | 优点 | 主要问题 | 结论 |
| --- | --- | --- | --- |
| 继续手工逐 half | 无新增代码 | 顺序、源码、板卡和收据靠人工；不可安全恢复 | 拒绝 |
| 单个不可中断大脚本 | 入口简单 | 一次失败会丢失长时间进度；恢复时容易重复或跳过 half | 拒绝 |
| 预注册 + 顺序状态机 + 不可覆盖收据 | 可恢复；每一步可独立审计；失败默认不完成 | 增加一个小型证据 schema 和 CLI | 采用 |

实现复用现有 staging/runner/harvest/analyzer/comparator/aggregator，只在工具层增加
冻结和收据边界。没有适用的外部协议或硬件接口需要重新定义；实体板身份来自 Linux
标准 `serial-number`/`machine-id`、hostname 和热区温度，板服务身份来自现有 xtask
分配输出。

## 分层和唯一事实来源

```text
clean Git commit + base rootfs + host C toolchain
              |
       prepare（构建一次）
              |
 preregistration.json（源码/制品/顺序/阈值）
              |
 run-next -> stage -> board-runner -> harvest -> analyzer
              |
 receipt.json（仅全部验证通过后独占创建）
              |
 五对 compare + 双 soak aggregate + checksums
```

设备地址、IRQ、控制器拓扑和虚拟设备实例仍由 AxVisor 当前
`ResolvedDeviceGraph` 解析和构建。正式活动仅冻结生成该运行时的源码、TOML、DTB、
kernel 和 rootfs 哈希；它不读取设备图内部状态，也不重新分配数字资源。

StarryOS 的 `lwprintf-rs 0.3.3` 在 `target_os = "none"` 时硬编码查找
`aarch64-linux-musl-gcc/ar`，但该路径只编译 `-ffreestanding -fno-builtin` C 对象并
生成 `core` bindings，不链接 musl libc。`prepare-freestanding-c-toolchain.sh` 因此
只在 RT 构建进程的 PATH 前生成受控命令别名，委托给已验证 target 为
`aarch64-linux-gnu` 的 GNU cross compiler/binutils；它不改变 Rust target、最终链接器
或运行时 libc。构建前会实际编译并归档一个 freestanding smoke object。

`host-toolchain.json` 冻结真实 compiler/ar 与生成 wrapper 的绝对路径、大小、SHA-256、
版本、target 和 header sysroot。预注册写盘前及每次 `verify` 都重新检查文件指纹并
执行 compiler/ar/wrapper 查询，因此依赖缺失、替换、target 漂移或 sysroot 不存在都
会在上板前 fail-closed。

`formal_campaign_contract.py` 拥有预注册 schema、测量合同和 analyzer 证据校验；
`formal_campaign_receipt.py` 拥有顺序状态机、不可覆盖收据及已完成 slot 的完整性
重验；`formal_campaign.py` 只是 CLI；`run-formal-campaign.sh` 编排已有边界。四者被
连同 host-toolchain 准备器一并列入 `source_inputs`，所以活动进行中修改任一实现都会
使后续 `verify` 失败。
编排器通过 `bash` 调用所有 shell helper；因此全新克隆会遵循脚本解释器合同，而不
依赖 helper 在 Git 树中是否带可执行位。集成测试固定这一约束，避免正式构建在上板
前因宿主文件模式差异退出。

## 状态、所有权与失败语义

活动总顺序固定为十二个 slot：五对按 AB/BA/AB/BA/AB 展开为十个 half，随后
shared、partitioned 两个 soak。`next_slot()` 只接受连续前缀：

- slot 没有 `receipt.json` 时仍未完成；已有收据必须再次通过 schema、源码、板卡、
  runtime marker、summary 合同及七个归档文件的大小/SHA 校验后才算完成；
- 后续 slot 出现收据而前一 slot 缺失时判为乱序并失败；
- 收据用独占写创建，既有文件绝不覆盖；
- 每次执行在 `attempts/<UTC>-<pid>/` 新建目录；失败只留下原始日志与
  `attempt-status.log`，不会推进状态；
- `run-next` 每次只消费一个 slot，`run-all` 只是重复调用它，因此可在进程中断后
  从同一结果根安全恢复；
- `aggregate` 是派生步骤；既有输出必须逐字相同，否则拒绝覆盖。

每个收据同时绑定：源码 commit/tree、stage 和 harvest 的实体板身份、板服务 ID、
console、summary、raw、guest IRQ、host trace 及其 SHA-256。summary 声明的三个输入
哈希还要与收据实际归档文件二次比对，避免“分析 A、归档 B”。

## 接受门与可观测性

预注册固定当前 M2 门槛：五对、直接 IRQ p99 非回归不超过 5%、直接 IRQ max 至少
四对改善且目标改善超过 10%、snapshot clean、每次恢复 Linux、所有 lossless
counter 为零。控制台还必须存在 capture/snapshot/restore marker，并不得出现 panic、
未处理 IRQ/异常或嵌套 vCPU 违规 marker。

关键机器可读 marker：

```text
AXVISOR_RT_FORMAL_PREREGISTERED
AXVISOR_RT_FORMAL_INPUTS_VERIFIED
AXVISOR_RT_FORMAL_STAGE_VERIFIED
AXVISOR_RT_FORMAL_RECEIPT_WRITTEN
AXVISOR_RT_FORMAL_SLOT_COMPLETE
AXVISOR_RT_FORMAL_CAMPAIGN_COMPLETE
```

完整命令见 `competition/reproduce.md`。单元测试覆盖脏树/制品漂移、板卡变更、
乱序收据、pair/soak 合同和归档哈希不一致；shell 集成测试保证正式入口、staging、
harvest 与基础 runner 合同继续连通。

## 回滚

工具不改变默认启动或运行时行为。回滚只需停止使用正式入口并删除一个尚未提交的
结果根；已经生成的收据和原始证据应保留以供审计。若正式活动失败，不允许降低阈值
或编辑收据，应修复根因、提交新源码，并在新的结果根上重新预注册。
