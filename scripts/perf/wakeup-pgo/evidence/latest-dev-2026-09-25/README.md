# 唤醒性能后续实验记录

本目录保存 PR #2477 之后的原始 full20 日志、实验状态和独立诊断。`resume771` 与
`resume773` 使用不同源码，不能把两者的差值解释为优化收益。两组镜像均
关闭 `ax-driver/rk3588-cpufreq` feature；独立 PLL 检查报告约 816 MHz。
该频率检查不等于在每个 full20 计时窗口同步采频。

## 1. 实验边界

完整日志保留 20 个场景的原始 p50、p99、p99.9 和样本计数；`status.json`
记录源码、镜像、benchmark SHA256、启动和判定。下列实验均未产生可保留的
调度器或 futex 运行时改动。

### 1.1 原生选择性 PGO

`resume771` 基于 `c346962754ff7a96359a928f07ffaedbcfeb9ca5`，仅对
`ax_task`、`ax_sched`、`ax_runtime` 使用原生训练的 profile。普通 release
A1 和无插桩 PGO F1 在 OrangePi-5-Plus-1 上各完成一次有效 full20：
每轮 20 项、380000/380000 样本、`not_parked=0`。F1 的 20 项中没有一项
达到冻结 Linux RT p50 的 90%；最差 OTHER `thread_futex_same_cpu` 为
20708 ns，对应 Linux RT 8458 ns，比例 40.84%。相对同源码 A1，p50、
p99、p99.9 分别有 11、11、10 项回退至少 3%。候选已拒绝，单次配对
只作淘汰筛查，不构成三次启动的验收。

### 1.2 最新源码普通基线

`resume773` 基于 `dev@05175ca38823b631a73777b0130226ddfa558439`，
使用普通 release、无 PGO、无 `qperf-metrics` 插桩。OrangePi-5-Plus-1 的
A1 有效 full20 包含 20 项、380000/380000 样本、`not_parked=0`；OTHER
`thread_futex_same_cpu` p50 为 27125 ns。该轮仅建立新源码对照，
**没有同源码候选，也没有 90% 验收结论**。普通镜像 SHA256 为
`b872b4b59829e0b63bd21a6d79c75295c5a4ee7fd470ef72afc8585a1976b8e5`。

### 1.3 四 crate 原生 PGO 训练前置修复

`resume779` 在相同 `dev@05175ca388` 上将 AArch64 C ABI `memset` 改为
不接受编译器插桩的 naked 汇编；非 AArch64 实现保持不变。原生
`ax_task`、`ax_sched`、`ax_runtime`、`starry_kernel` 四 crate 的
profile-generate 镜像在 OrangePi-5-Plus-1 启动到 Starry shell。
训练镜像 `.bin` SHA256 为
`7165206f4851cf923212ee2de2dea28b86dd0ec42dd9c34e23eb7d600227f653`，
串口原始记录 SHA256 为
`4c05ee9633f0d49106229aca071a825fa7f53392953b251b3f942decc1965b64`。

`resume780` 临时启用 debugfs profile exporter 后取得四 crate 原生计数，
profdata SHA256 为
`7d71a724fd5ce66e4faf15280995602392fc6ff8e6d41fc197cdbc0eb8e67156`。
16 个定向训练场景都运行至完成标记，但四次 OTHER 同核 futex 各有
`19999/20000` 样本和 `not_parked=1`；这份训练记录不满足 full20 验收。
exporter 已撤回，未纳入 PR 源码。`resume781` 的 profile-use 构建因
训练 feature 改变 `starry_kernel` 身份，出现 14792 个缺失 profile 警告，
未作为候选上板；`resume782` 的严格符号/CFG 哈希映射也无法覆盖最热的
`sys_futex` 和 `ResolvedFutex::wait_nofault_until`，因此未把部分映射
当作已解决的训练方案。

`resume783` 在上述 `memset` 源码上完成十项 feature、无 PGO、无插桩
的普通 release 构建；`.bin` SHA256 为
`4d2e6656ff13684de1ae0202e25c150a027bc6ee504facd479cf9eed566c31a4`。
反汇编核对 `memset` 的零长度返回、逐字节写入和原指针返回；ELF 无
`__llvm_prf_*` section。`cargo fmt` 和 `git diff --check` 通过；
`cargo xtask clippy --package starry-kernel` 展开 72 组配置，前 10 组
通过后因磁盘不足主动停止，**不能报告全量 clippy 通过**。普通 release
尚未上板，未取得同源码 A/F full20，不能声称性能改善或 `<3%` 回退门通过。

### 1.4 匹配 crate 身份的四 crate PGO 筛查

`resume785` 和 `resume786` 基于 `dev@05175ca388` 加相同的 AArch64
`memset` 修复（实验提交 `69a3365076`；对应 PR 提交 `34cb1a14a3` 的
`kprint.rs` 内容相同），并在**两侧**应用相同的临时 debugfs exporter
补丁（SHA256 `d7c1388dec5a849b1bec41e49b51d1f4cea6e2e853f1b4b2ce58d195a71d2b78`）。
两侧均使用去掉 cpufreq 的十项 OrangePi feature 加
`starry-kernel/board-profile-export`，保持 `starry_kernel` crate 身份一致。
训练 profile SHA256 为
`7d71a724fd5ce66e4faf15280995602392fc6ff8e6d41fc197cdbc0eb8e67156`；
候选仅对 `ax_task`、`ax_sched`、`ax_runtime`、`starry_kernel` 使用它。
普通与候选的 `.bin` SHA256 分别为
`750589429f5afe81b16775dde50050fedb936614b78d7bbde999a51856618473`、
`e48394c7ef55008d0badf2cd6dc841fa8fa6593e069720304e2400634614fd6d`。
两侧 `AXTEST_COVERAGE=1` 只使链接器提供空的 profile 起止符号；
镜像没有非空的 `__llvm_prf_*` 计数段，full20 无采样插桩。
候选不再出现 `starry_kernel` 热路径缺失 profile 的警告，但
`ax_runtime::run_idle` 仍有一项部分忽略警告。构建状态见 `resume785/`、
`resume786/`；临时 exporter **没有进入 PR**，因此这些实验镜像不能
直接由当前 PR 的生产构建入口复现或作为最终候选。

`resume787` 在 OrangePi-5-Plus-1 按 A1→F1 运行冻结 full20。A1 有效：
20 项、380000/380000 样本、零 `not_parked`。F1 的 OTHER
`thread_futex_same_cpu` 只有 19999/20000 样本，`not_parked=1`，
**整轮无效**；日志末尾的 `WAKEUP_LATENCY_PASSED` 不改变此判定。
`resume788` 独立重启同一候选 F2，20 项、380000/380000 样本、零
`not_parked` 和 `missed_deadlines`，是一次有效的候选 full20。

只把有效的 A1/F2 作为**单次探索性对照**：F2 有 11/20 项达到冻结
Linux RT p50 的 90%；最差 OTHER `thread_futex_same_cpu` 为 16625 ns，
Linux RT 为 8458 ns，比例 **50.88%**（A1 为 27417 ns）。该单次对照的
20 项 p50、p99、p99.9 均未见达到 3% 的回退，但缺少足够的有效启动，
不能宣称回退门通过。包括 FIFO timer、四项跨核 futex、OTHER timer、
OTHER yield handoff、OTHER 同核 futex 在内的九项仍低于 90%。F1 的
部分行数据只供诊断，不拼接进 F2 验收。完整逐项数值在 `analysis.json`
及原始日志中，`check.py` 逐项复算。

上述 PGO 阶段仅归档了构建与板测筛查，**没有可保留的新运行时逻辑优化**。
需先移除临时 exporter 对生产构建的依赖，重新建立可复现的同源码
普通/PGO 对照，处理无效轮次和九项缺口，并完成既定三次有效候选
full20 与 `<3%` 回退门；PR 保持草稿。

### 1.5 IRQ 描述符索引试验

`resume795` 在相同 `dev@05175ca388` 和早期 `memset` 修复上，只试验
`irq-framework` 的分发收尾：`Registry::begin_dispatch()` 返回描述符索引，
`DispatchGuard` 将它交给 `Registry::end_dispatch()`，省去一次线性查找。
源代码和并发注册测试保存在 `resume795/` 的补丁中，**未应用到 PR 的
运行时源码**。补丁采用零上下文格式，在其父提交 `69a3365076` 上需用
`git apply --unidiff-zero --check` 复核。普通 A1 与试验 B1 均关闭
cpufreq，使用相同的临时 exporter feature；均为无 PGO、无
`qperf-metrics` 的 release 镜像。两次独立启动
各完成 20 项、380000/380000 样本、零 `not_parked`，原始日志与镜像
SHA256 见 `resume795/results.json`。

同源码 A1/B1 中，OTHER 同核 futex p50 为 27417→26834 ns，改善 2.13%，
但 B1 仅 **10/20** 项达到冻结 Linux RT p50 的 90%。相对 A1，FIFO
`sched_yield_handoff` p99.9 回退 5.25%，OTHER `absolute_timer_same_cpu`
p99.9 回退 54.53%，OTHER `sched_yield_no_peer` p50 回退 11.09%。
试验已否决，不把单项 p50 改善计入可保留收益；一次配对足以否决该候选，
不能用于接受候选或证明最终回退门。`check.py` 从原始 full20 日志复算
样本完整性、20 项性能和上述回退。

### 1.6 唤醒路径诊断（不作为性能验收）

`resume799` 在 `dev@05175ca388` 加 `memset` 修复的源码上临时插入
`qperf` 计时探针，原样压缩保存未采用的 `probe.patch.gz`、计数器快照及原始串口
日志。OrangePi-5-Plus-1 的 FIFO/OTHER 跨核线程 futex 各两轮均为
20000/20000 样本、零 `not_parked`；`notify_cpu` 真正发送 IPI 的
探针均值分别为 FIFO 1469.83–1474.50 ns、OTHER 1701.55–1711.16 ns。
这是带插桩的聚焦诊断，**其 p50 不能与无插桩 full20 或 Linux RT
基线直接比较**；探针已撤回，没有 IPI 运行时改动。

`resume801` 复用 `dev@05175ca388` 的另一张旧 `qperf` 镜像，只对
同核线程 futex 读取前后计数器。FIFO 两轮和 OTHER 第二轮有效；
OTHER 第一轮仅 19999/20000 样本、`not_parked=1`，已排除。有效轮次
每样本 context switch 为 FIFO 2.1061–2.1298、OTHER 2.1969；
owner scheduler-rq 事务为 FIFO 3.1623–3.2060、OTHER 3.2543，
不支持“OTHER 多一次完整切换/事务”的解释。OTHER direct-wake
preemption 为约 1.050/样本，FIFO 为 0.0026–0.0144/样本。
这些都是全局探针计数，不能据此推出原生 p50 可获得的收益。
两次诊断使用**不同镜像**，彼此的 p50 也不能配对。

`resume802/status.md` 对照旧的 Fair 抢占分段探针：当前相关源码文件
未变，但旧插桩均值既不是当前机器码成本，也不是可删除成本。
F2 最差 OTHER 同核 futex 的 90% 上限是 9397 ns，距 16625 ns
还差 7228 ns；目前没有证据支持跳过抢占语义或保留单点微优化。
以上材料只定位下一步实验范围，**没有新增可保留的运行时优化**。

### 1.7 同核 Fair 路径审计

`resume806` 只读核对了 OTHER `thread_futex_same_cpu` 的唤醒和后续
抢占调度路径，没有修改源码、构建镜像或运行板测。`wake_thread_source()`
按被唤醒任务的 rq 成员状态走 `wake_on_rq_locked()` 或直接激活路径；
两者在 owner rq 事务中结算 Fair 状态并可能发布 Lazy 抢占请求，用户态
返回前的 `prepare_user_return()` 消费该请求。普通 Fair 当前任务的
`prepare_owner_rq_schedule_out()` 已使用 rq-only 路径，不存在可删除的第二次
task-lock rq 事务。

唤醒时的 `earliest_eligible()` 只是抢占分类查询；调度时的 `pick_eligible()`
发生在 `put_prev_unlinked_current()` 将当前任务重新入队之后。两次 rq 事务
之间允许中断和其他唤醒，因此不能直接复用前一次 Fair 候选而保持 EEVDF
选择及切片保护语义。同一次唤醒事务里可能存在一次输入相同的 Fair V
重锚，但旧插桩叶子均值只有约 234–261 ns 且包含探针开销；旧 `exp120`
的另一项 Fair V 刷新删减也因无效轮次和收益不足被拒绝。它们都不能解释
F2 的 OTHER 同核 futex 距 90% 上限 9397 ns 尚差 **7228 ns**。以上是
源码审计和旧插桩诊断，**没有新的原生性能收益或可保留的运行时改动**。

`resume807` 继续只读核对私有 futex 到用户态返回、上下文切换的共用路径。
私有 futex key 检查已是地址范围检查，`ThreadWakeBatch` 无每次分配，
同一 mm 的切换也已跳过地址空间激活。旧分段数据来自插桩镜像，不能把
其中某段耗时直接算作 F2 可删除的原生耗时。此次未找到有证据支持、且能
覆盖多项缺口的多微秒单点改动；后续 `resume808`–`resume810` 已开展
PMU 诊断，见下节。本节不增加性能验收轮次。

### 1.8 同板 PMU 诊断（不计入 full20）

`resume808-current-pmu/` 在相同 `dev@05175ca388` 加 `memset` 修复上，
使用 `resume788` 的同源码普通 A/PGO F 镜像，按 A1→F1→F2→A2 顺序运行。
原冻结 benchmark SHA256 为
`94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773`；
固定窗口 PMU 采集器 SHA256 为
`47b89a6df65277ef7e0b3c26f6dc1c2a49d019219df7a92fbad2b39b135d1394`。
每次仅运行 OTHER/FIFO 同核线程 futex，采集 CPU0 内核态指令、周期、L1I/L1D
refill。24 次中有三次 OTHER 为 19999/20000、`not_parked=1`，已保留原始日志
但排除；有效轮次的整段计数显示 F OTHER 约 16761 指令/attempt、CPI 1.589、
每千指令 40.08 次 L1I refill，F FIFO 分别为 15577、1.368、20.20。

`resume809-linux-pmu/` 在同一 OrangePi-5-Plus-1 上用冻结 Linux v7.1
PREEMPT_RT 镜像和相同 benchmark/采集器完成三次 OTHER、三次 FIFO，均有效；
整段 OTHER 为 15429 指令/attempt、CPI 1.449、每千指令 24.40 次 L1I
refill。整段包含启动、反向交接和背景活动；F OTHER 诊断 p50 约为 Linux
的 2.02 倍，但指令/attempt 仅为 1.09 倍，不能用整段计数归因正向唤醒。
两轮的原始日志、镜像哈希、运行顺序、无效样本和复算分别见子目录的
`status.json`、`decision.md`、`analyze.py`；一次性二进制与 initramfs 未入库。

`resume810-window-pmu/` 改用同一个**诊断版** benchmark（SHA256
`7ea6ace6192fb9ffafc2cd0ce13c2c5cc97ceb7eb6808e282c0a417e7ffa82b6`）
比较 Linux RT 与 Starry A/F；这是修改了计数窗口的 benchmark，不是冻结
full20。采集窗口从发送者写入时间戳前到接收者首次读时钟后，每轮只计一个
CPU0 内核态事件，并用相邻 PMU read 估计读数开销。Starry 顺序仍为
A1→F1→F2→A2。唯一无效轮次为 Linux L1D OTHER 第二次，
19999/20000、`not_parked=1`，已排除。近似扣除相邻读数后，各有效轮次中位数：

| 事件/attempt | Linux OTHER | Linux FIFO | Starry F OTHER | Starry F FIFO |
|---|---:|---:|---:|---:|
| retired instructions | 4992 | 5305 | 8889 | 7044 |
| CPU cycles | 8818.5 | 8114 | 15444 | 10890 |
| L1I refills | 224 | 155 | 456 | 257.5 |

F 的 OTHER/FIFO 差值比 Linux 多约 2158 指令、3849.5 周期和 129.5 次
L1I refill，提示继续检查 Fair 唤醒/抢占路径。首次 PMU read 与相邻校准
read 的缓存状态不同、原始逐样本计数未导出、两侧背景活动不同，因此这些
**不是精确可删成本，也不是新的 p50 性能收益**。`handoff.c` 仅是诊断副本，
未改变生产 benchmark 或内核运行时源码。原始 80 次诊断调用、无效轮次、
构建与镜像身份、复算脚本和完整串口记录均保存在三个子目录；
`resume810-window-pmu/decision.md` 说明测量边界。

最近一次有效、原 benchmark、无插桩 full20 仍是 `resume788` F2：
**11/20** 项达到冻结 Linux RT p50 的 90%，最差 OTHER 同核 futex
16625 ns / Linux RT 8458 ns，即 **50.88%**。三次有效候选启动、同源码
`<3%` 回退门及生产构建复现仍未完成；当前没有新的可保留运行时优化。

### 1.9 唤醒顺序与同二进制分层 PMU（仅诊断）

`resume811-wake-order/` 使用同一份静态诊断 benchmark
（SHA256 `3c72c5ead9c0aad2e0bd271f244807f16cad206d125141199fa52deab86e6189`），
在发送者 `futex_wake_one()` 返回后的第一处 C 语句设置标记，并由接收者在
首次读时钟后观察。Linux RT 的两次有效 OTHER 聚焦轮次中，接收者先于
标记的中位比例为 **63.40%**；同源码 Starry 普通 A/PGO F 各五次有效
OTHER 轮次分别为 **99.19%/99.385%**，FIFO 全为零。Linux OTHER 一次、
Starry A1/F1 OTHER 各一次出现 19999/20000、`not_parked=1`，原始日志保留但
不计入中位数。标记不能区分唤醒 syscall 内抢占与返回用户态前的抢占；
两边使用的是改变代码布局的诊断 benchmark，其 p50 不计入 full20。

`resume812-audit/` 修正了只读路径审计中的错误假设：Starry 的
`prepare_user_return()` 在首次唤醒 syscall 返回前处理 Lazy 抢占请求，
不能认为测得的正向窗口通常包含发送者第二次 `FUTEX_WAIT`。旧全局探针
提示成功唤醒多走 off-rq 激活，但不能将全局计数换算成逐样本成本。
此步未修改内核，也未找到可保留的等价运行时优化。

`resume813-stratified-pmu/` 在同一静态诊断 benchmark（SHA256
`75939fe5a8ccd15476e1f15f967b11c19e5bb680df3da39be141ab391cb73b76`）
中同时采集上述标记及 CPU0 正向窗口的内核态指令、周期、L1I refill，
分别在冻结 Linux RT 和同源码 Starry A/F 上运行。Linux 每项/策略两轮，
Starry 按 A1→F1→F2→A2 各两轮，共 60 次聚焦调用，全部 20000/20000、
零 `not_parked`。在指令事件组的接收者先于标记 OTHER 分层中，Linux RT/A/F
的中位样本数分别为 18896/16561.5/19873.5（每轮 20000）；相邻读数近似校准后，
指令中位数为 4992/11104/8889，周期为 8506.5/24753/15610，L1I refill
为 224.5/810/454。**加入 PMU 读数把 Linux RT 的标记先后比例从上一版
63.40% 改到约 95%，也改变了 Starry A 的比例**，诊断明显扰动了交接节奏。
这些数值只支持在当前插桩条件下继续定位，不能解释为原始 benchmark 的
可删除成本或候选优化收益。没有导出逐样本 PMU 原始值；首次读数与相邻
校准读数的缓存状态也可能不同。完整限制见 `decision.md`。

三个目录保留源码副本、运行脚本、原始单项/串口日志、启动状态、镜像和
benchmark 哈希、无效轮次及独立复算；一次性静态二进制和 Linux initramfs
未入库。原始冻结 full20 最近一次有效轮次仍为 `resume788` F2：**11/20**
达到 90%，最差 OTHER 同核 futex **50.88%**；三次有效候选启动、同源码
`<3%` 回退门及生产构建复现均未完成，PR 应保持 Draft。

### 1.10 最新源码交接路径审计

`resume814-815-path-audit/decision.md` 复核普通 futex 的实际唤醒入口、
同核 Fair/RT 交接及旧 timer、FP/SIMD、PMU 诊断边界。常见 futex 唤醒
不走 wait-queue claim；旧 timer 计数不支持每次同核唤醒都重编程硬件，
旧 `prepare_arch` 插桩总段也无法解释九项的多微秒差距。两轮均为只读
审计，**没有源码改动、镜像、板测或新增 full20 收益**。现有 PMU
数据也没有逐 PC/调用栈归因，不能从聚合计数推断可删除成本。

### 1.11 重新加权的四 crate PGO 筛查

`resume817-819-weighted-pgo/` 保存 `dev@05175ca388` 加已提交 `memset`
修复后的另一份原生 PGO 训练、构建和两次独立 full20。训练仍沿用
`resume780` 的临时 exporter 镜像，但将工作负载改为 20 项全跑一次，
再额外运行 OTHER 同核 futex 七次；训练产生 17 个 `not_parked`，
不用于验收。`wrapper.jsonl` 确认四个目标 crate 使用新 profile，
`starryos` 未使用。候选无 profile-generate 插桩，`.bin` SHA256 为
`9e9847a433cd99808d7f511372d454eb2cf5412a94e42187bcab64780ebfb8a3`。
profile 和镜像二进制仅保存在本地实验归档，本 PR 留存哈希、构建日志、
训练记录和逐项原始日志；临时 exporter 未进入 PR 运行时源码。

G1/G2 在 OrangePi-5-Plus-1 的独立启动均完成 20 项、380000/380000
样本，零 `not_parked` 和 `missed_deadlines`。两轮逐项 p50 中位数仍只有
**11/20** 达到冻结 Linux RT 的 90%，最差 OTHER 同核线程 futex 为
14583.5 ns / RT 8458 ns，即 **58.00%**；OTHER timer 为 **58.88%**。
旧 profile F2 的最差值为 16625 ns，重训在该项有局部 p50 改善，
但 FIFO timer 和 OTHER 两项跨核 futex 的 p99.9 相对 F2 分别增加
6.32%、18.11% 和 22.36%。与**仅一次**同源码普通 A1 比较没有
`>=3%` 的逐项回退，仍不足以通过多次启动回退门。完整九项未达值、
profile/benchmark/原始日志 SHA 及生产构建限制见子目录 `decision.md`
和 `analysis.json`。**没有可提交的运行时优化；本候选已拒绝，PR 保持 Draft。**

### 1.12 后续交接与 CPU1 IPI 诊断

`resume829-833-path-diagnostics/` 归档了最新源码上的两次只读审计和三次
临时 `qperf-metrics` 板测。`resume829` 纠正旧探针只记录首个 `IrqReturn`
帧的覆盖缺口；`resume830/831` 将实际 ktimer worker 切入与同 generation
通知、claim 配对。OTHER 有效轮次的通知返回至调度帧入口、帧入口至
worker 选择、选择至 claim 分别主要落在 4–6、6–8、4–6 us 的 2 us
直方桶。这些区间包含必需的调度、切换和探针开销，不能相加或视为可删除
成本。`resume832` 确认 Fair 同核路径已有单次 owner-rq 事务，没有证据
支持删去第二次事务或无条件省略 idle tick fallback。

`resume833` 在 OrangePi-5-Plus-2 对 CPU1 跨核 futex IPI 的派发包围段和
注册 handler 计时。FIFO/OTHER 各两轮均为 20000/20000 样本、零
`not_parked` 和 missed deadlines；每轮派发与 handler 计数一致。包围段
平均比 handler 多 1181–1247 ns/次，但还包括 per-CPU pin、分发包装和
测量开销，**不是 IRQ registry 的独占成本**。这段差值本身也不足以
解释原生跨核项约 8–9 us 的缺口，因此没有保留 registry-only 修改。
`resume830/831` 各四轮 timer 聚焦数据同样全部有效，但均带插桩。

归档保留三次板测的探针补丁、原始日志、前后计数器快照、结果和镜像哈希；
镜像本体只在本地实验归档，不在 PR 中。`check.py` 逐轮核对原始日志、
样本、计数器差值和 IPI 直方图。三次临时探针已从工作源码撤回，
**本阶段没有新的运行时改动或无插桩 full20 收益**。最新有效 G1/G2
仍为 11/20 达到 90%，最差 OTHER 同核 futex 为 58.00%。

### 1.13 跨核路径和频率复核

`resume834-836-sgi-path/` 保存两次只读审计和一次临时插桩板测。`resume834`
逐段核对跨核 futex 的唤醒、物理 SGI、CPU1 IRQ 和调度返回路径；此前
`resume833` 的派发包围段差值不能归因于 IRQ registry，也不足以解释跨核
p50 的数微秒缺口。`resume835` 对照旧 18/20、已打包 19/20 和最新 G1/G2
的构建身份：前两组启用了 cpufreq feature，已打包镜像另测得约 1150 MHz；
冻结 Linux RT 和 G1/G2 的独立探针约为 816 MHz。旧 18/20 镜像没有直接
测频，不能把旧结果当作同频率 90% 进展，也不能把跨源码差值归因于逻辑回退。

`resume836` 在 OrangePi-5-Plus-2 将 CPU0 发送前与 CPU1 识别 SGI 后的
时间配对。FIFO/OTHER 各两轮 `thread_futex_cross_cpu` 均为 20000/20000
样本、零 `not_parked` 和 missed deadlines；每轮约 2.1 万个 CPU0 配对，
无覆盖写入、未配对或反向时间。250 ns 直方图的中位桶为 FIFO
2750–2999 ns、OTHER 3000–3249 ns。该区间包含发送包装、GIC 投递、
异常入口和 `begin_irq()`，且混有非 benchmark IPI；不能与原生 p50
相减，也不能算作新性能收益。原始日志、计数器快照、探针补丁、配置和
镜像哈希均已归档；临时探针已撤销，镜像只保留在本地实验归档。
最新有效、无插桩同频率 G1/G2 仍是 **11/20** 达到 90%，最差 **58.00%**。

### 1.14 当前 CI 边界

原 PR head `1689312780` 的 [CI run 36054555037](https://github.com/rcore-os/tgoskits/actions/runs/36054555037)
中 `Starry / Board OrangePi 5 Plus · Suites` 已失败：
`native-network-smoke` 的用户态 `iperf` 触发 SIGSEGV，读取地址
`0x2e3836312e323941`。精确 `dev@05175ca388` 的
[run 36025857970](https://github.com/rcore-os/tgoskits/actions/runs/36025857970)
在同一用例出现相同失败签名。只能说明签名早于本次证据提交，
不能证明根因相同、把失败计作通过，或代替新 head 的 CI 结果。

后续 PR head `fedad594b0` 的
[CI run 36059377315](https://github.com/rcore-os/tgoskits/actions/runs/36059377315)
已经结束，`Starry / Board OrangePi 5 Plus · Suites` 的
`native-network-smoke`、`Starry / Board AKA-00 SG2002 · Suites` 的
`wifi-iperf-smoke` 和 `ArceOS / Board OrangePi 5 Plus · CPU PMU` 均失败。
前两项出现用户态 SIGSEGV；CPU PMU 任务在等待 U-Boot shell 时超时。
通过的其他任务不能覆盖这些失败，下一次文档提交触发的新 CI 也须按其
精确 head 重新核对。PR 继续保持 Draft。

PR head `145b3f349f` 的
[CI run 36092724049](https://github.com/rcore-os/tgoskits/actions/runs/36092724049)
也已结束：Starry OrangePi 5 Plus 的 `native-network-smoke` 和 AKA-00
SG2002 的 `wifi-iperf-smoke` 各因用户态 SIGSEGV 失败。其余该 run
列出的任务通过；两个失败不能计作通过。OrangePi 的同用例故障签名
此前已在精确 `dev@05175ca388` 上出现，但尚未证明两次故障根因相同。

### 1.15 本地强制抢占判别

`resume837-842-local-path/` 归档跨核 SGI 后半段、同核 futex、定时器
worker 的只读审计，以及在同一 OrangePi-5-Plus-2 上以相同用户态
二进制执行的 Starry/Linux RT 聚焦对照。接收者优先级由 FIFO 80
提高到 81 后，两侧 p50 均改善，但 RT/Starry 中位比值从等优先级
的 71.96% 降至 60.81%；OTHER 对照 Linux 第二轮仅 19999/20000
样本、`not_parked=1`，已排除。这个结果指向 Fair 之外的共用
即时抢占与切换路径，却没有定位可删除的多微秒操作。只读源码审计
未发现具备语义证明和收益上界的事务级候选。该变体不计入 full20，
没有运行时改动或新的 90% 收益。原始串口、二进制、源代码、
`analyze.py` 和哈希核验方式见子目录 README。

### 1.16 命中唤醒策略轴诊断

`resume850-852-policy-axis/` 归档命中唤醒路径只读审计、一次无效试验及
有效的 Starry/Linux RT 同二进制对照。`resume850` 没有找到可证明安全且
可能节省数微秒的 domain/batch 局部改动；同地址 requeue 不执行完整的
等待者移除、状态转换和调度器激活，不能充当唤醒成本替身。

`resume851` 的 50 us park settle 在 Starry 两次启动中都未取得完整有效
六轮：第一次 runner 在下载逐轮日志前断言，第二次保留了所有原始日志，
其中 OTHER 第 3/6 轮分别有 3/2 次异常返回。已整体判无效，不能拼接
其中成功的 FIFO 和 OTHER 轮次，也没有用这组结果与 Linux 比较。

`resume852` 仅将计时窗口外的 park settle 延长到 500 us，并将异常
唤醒返回按空、不匹配、匹配三臂分别计数。相同静态二进制 SHA256
`2cbc88477cd5ff43475505534918c5c2ee9835de33c916d4c8203b1f9babd19b`
在 OrangePi-5-Plus-2 的未改动 Starry G 镜像及冻结 Linux RT 镜像上
各启动一次，每侧按 FIFO、OTHER、OTHER、FIFO、FIFO、OTHER 顺序
独立运行六个进程。CPU0 FIFO80 发送者不变，CPU0 接收者为 FIFO1 或
OTHER；每轮三臂各有 1000 次预热和 20000 次有效样本，唤醒返回
始终为 0/0/1，策略、CPU、退出状态和板卡释放均已核对。

按每轮“命中 p50 减不匹配 p50”再取策略内三轮中位数，Starry 的
FIFO/OTHER 为 4958/5833 ns，Linux RT 为 2042/3209 ns；OTHER
相对 FIFO 的增量分别为 875/1167 ns，Starry 特有增量为 **-292 ns**。
这未达到预登记的 Starry 至少多 1000 ns、Linux 不超过 300 ns 的
Fair 发送侧假设门槛。因此，**低优先级、不会立即抢占发送者的唤醒
窗口不足以解释 full20 最差项的策略差**；不能由此确定 park、切换或
恢复中的具体原因。该变体改变了冻结工作负载，三轮进程也不是三次
独立启动，所有数值只用于诊断。原始串口与逐轮日志、协议、构建脚本、
二进制、镜像哈希、`SHA256SUMS` 和 `resume852-settle/check.py` 均在目录中。
这次没有运行时源码改动，也没有新的无插桩 full20；最新有效 G1/G2
仍是 **11/20** 项达到 90%，最差 **58.00%**，PR 继续保持 Draft。

### 1.17 Fair yield 阶段诊断

`resume853-855-fair-yield/` 保存当前 `69a3365076` 源码的交接路径审计与
`resume854` 同板聚焦诊断。只读审计确认发送侧唤醒与后续 park/切换属于
不同 owner rq 事务，不能跨事务复用 Fair 选择结果；也不能把不同 benchmark
场景的边缘 p50 相减当作可删除的耗时。诊断直接使用已有的
`yield_run_queue.rs` qperf 阶段计数器，未修改生产运行时源码。

`resume854` 的 `qperf-metrics`、无 PGO、cpufreq-off 镜像在
OrangePi-5-Plus-2 上启动一次，按 FIFO/OTHER/OTHER/FIFO/FIFO/OTHER
运行六个独立进程。每轮仅执行 `sched_yield_handoff`，均有
20000/20000 样本、零 `not_parked` 和 `missed_deadlines`，所有五段计数
在每轮内完全相等，约 42008–42010 次。每事件插桩均值的策略内中位数中，
OTHER 相对 FIFO 的 `put-prev`、`pick` 分别增加 2218.75、937.74 ns；
`rq commit`、`selection tail` 分别增加 774.00、658.39 ns。预登记的
前两段合计差 3156.49 ns/event，大于后两段合计的 1432.40 ns/event，
因此后续优先检查 Fair 当前任务重新入队与挑选路径。

这些是包含探针且可能混有后台调度的全局计数；镜像内 FIFO/OTHER
benchmark p50 中位数为 10500/16917 ns，明显高于无插桩 G1/G2 的
4083/7583 ns。阶段差值**不是原生可删除成本**，也不是 Linux RT 对照
或 full20 性能收益。原始串口、每轮日志、计数器快照、构建配置和复算
脚本已归档；仓库副本的构建日志只规范化了 CRLF 行尾。临时 `image.bin`
未纳入 Git，镜像 SHA256 为
`255c03696d0de9b808c6c06a9d3cf61303557212c602253016ae500f2e0877fc`。
仓库内校验可复核日志与镜像身份记录，不能重算缺席镜像本体的哈希。
`resume855` 又核对了两任务 Fair yield 的 EEVDF 入队、选择及发布边界：
未找到可跳过且有多微秒收益证据的等价操作；旧 retained-node 表示法
已经在 `resume135/136` 原生测试中退化。独立审计报告把不同场景、
插桩与原生 p50 换算成可删上界的推导不成立，主审结论已在
`resume855-audit/decision.md` 逐项限定。被选任务的 hrtick 派生复用
已在后续 `resume856` 源码审计中证伪，见下一节。
最近有效无插桩 full20 仍是 **11/20** 达到 90%，最差 **58.00%**；
PR 继续保持 Draft。

### 1.18 Timer worker 通知分类

`resume856` 在 `69a3365076` 上核对 `SchedulerDeadlineRqObservation` 的
选择和发布：rq 未变化时已有一次派生值传递；只有负载均衡改变 rq 时才
重新观察，不能复用旧结果。`resume857` 核对公共 context-switch 尾段与
`resume854` 计数：已测的 validate、trace 和 out hook 均值是含插桩的
亚微秒全局事件，未发现有语义依据的多微秒可删操作。这两次是只读审计，
没有代码或板测改动；结论保存在本地实验台账的 `resume856/857` 记录中。

`resume858-ktimer-fresh/` 基于同一源码，给现有 generation 配对的
`ktimers/%u` 通知探针增加 `WakeResult::Notified` 与
`WakeResult::AlreadyPending` 分类；`probe.patch.gz` 保留临时 qperf 插桩原件，
未进入生产源码。OrangePi-5-Plus-2 的一次启动运行 FIFO、OTHER timer
各两个独立进程，每轮 10000/10000 样本、零 `not_parked` 和
`missed_deadlines`，PLL 检查和板卡租约释放均通过。下表是 CPU1
hard-IRQ 通知计数前后差值，以及同 generation 的通知到 worker claim
直方图中位桶。

| 策略 | 轮次 | Fresh | AlreadyPending | Pending | 配对数 | p50 桶 |
| --- | ---: | ---: | ---: | ---: | ---: | --- |
| FIFO | 1 | 504 | 0 | 0 | 504 | 16–18 µs |
| FIFO | 2 | 491 | 0 | 0 | 491 | 16–18 µs |
| OTHER | 1 | 11443 | 0 | 16 | 11443 | 16–18 µs |
| OTHER | 2 | 11437 | 0 | 6 | 11437 | 16–18 µs |

四轮中 `AlreadyPending` 都为零，因此这张镜像上先前的粗粒度
`Notified` 计数没有掩盖大量合并通知。FIFO 的 worker 通知是背景事件，
因为 FIFO park 使用 `ParkHard`；OTHER 的 `ParkSoft` 才经过 timer worker。
16–18 µs 包含 IRQ 剩余工作、调度、切换和 wait 返回，也包含探针开销，
**不是原生可删成本或性能收益**。这次没有新的无插桩 full20；最新有效
同频率 G1/G2 仍是 11/20 项达到 90%，最差 OTHER 同核 futex 58.00%。
三次候选启动、同源码 p50/p99/p99.9 全项 `<3%` 回退和生产构建门禁
仍未完成，PR 保持 Draft。

### 1.19 最新 dev 同核 futex 唤醒分段

`resume862-futex-wake-stages/` 基于 `dev@714accd8f6` 加现有 `memset`
训练修复的 `b292a098bb`，仅在 `qperf-metrics` 下临时插桩；
诊断镜像无 PGO、关闭 cpufreq feature，未纳入 Git。OrangePi-5-Plus-1
同一次启动按 OTHER/FIFO/OTHER/FIFO/OTHER 运行五个独立同核线程 futex
进程，逐轮均为 20000/20000 样本、零 `not_parked` 和
`missed_deadlines`。冻结 benchmark、镜像、源码快照和原始串口均有
SHA256 记录。临时插桩已撤回，PR 生产运行时没有变化。

OTHER 三轮命中且入队的唤醒为 21465/21454/21440 次；即使把各轮
所有全局 direct-wake 未激活都归于目标 gate，目标 off-rq 激活比例
仍至少为 97.645%。锁前至批量前的插桩调用均值为 1657/1676/1652 ns，
`wake_batch` 区间为 9891/9977/9875 ns。FIFO 的对应区间为
1386/1360 ns 和 5416/5407 ns。**这些均值含探针，且全局抢占计数
未与单次唤醒配对；不能把 `wake_batch` 差值或插桩 p50 折算为
原生可删成本。** 独立源码审计发现 Fair 命中唤醒通常发布 Lazy
请求，不能凭每轮约 21570 次 `context_switches_preempted` 推定
切换发生在 `wake_batch` 内。后续应先定位 Lazy 消费点和
park/恢复交接，再提出保留语义的原生候选。

`cargo xtask clippy --package starry-kernel` 的 72/72 组配置及
`cargo xtask test` 的 68/68 项白名单已通过；五轮原始日志由
`python3 check.py` 离线复算。此诊断**没有新的无插桩 full20
性能收益**；旧源码 G1/G2 仍是最近有效的同频率 90% 比较，
仅 11/20 达标，最差 OTHER 同核 futex 58.00%。三次候选启动、
同源码 p50/p99/p99.9 全项回退和生产构建门仍未完成，PR 保持 Draft。

### 1.20 最新源码 PGO 构建门禁

`resume872-current-pgo-audit/` 在 PR head `ac9f443d5e` 上以
`scripts/perf/wakeup-pgo/build.py --prepare-only` 复核生产构建入口。
`build-result.json` 记录 `WAKEUP_PGO_BUILD_NOT_READY`：脚本仍锁定
`dev@9a7b868bab` 与旧训练提交 `4322e0c505`，最新源码在排除文档和
性能证据路径后仍有 28 个路径与其不同。锁文件哈希虽匹配，旧 profile
也不能据此跨源码复用；该次检查在构建和上板之前停止。

当时计划在 `dev@714accd8f6` 加现有 `memset` 训练修复的
`b292a098bb` 上重新训练。训练专用的 `board-profile-export` 只用于
采集精确 ELF 的计数器；最终候选必须不含 exporter，使用关闭 cpufreq
的十项板卡 feature，且只对 `ax_task`、`ax_sched`、`ax_runtime`、
`starry_kernel` 应用新 profile。训练镜像的 `.text` 因插桩本来就不同于
普通 release；应核对的是 profile 计数与训练 ELF 的布局，以及最终
无插桩镜像和上板镜像的字节身份。本节的构建门禁审计尚无新候选、
full20 或 90% 验收结果，生产构建入口也尚未更新。

### 1.21 最新源码 root-only exporter 与无插桩 PGO 筛查

`resume873-876-current-pgo/` 记录 `dev@714accd8f6` 加现有 `memset`
修复后四次探索。`resume873` 已取得训练计数，但 exporter 在被 profile
的 `starry_kernel` 内；`resume874` 的空 feature 和 `resume875` 的
`--cfg` 尝试都未消除热 futex 函数的 profile CFG 不匹配，未上板验收。

`resume876` 把训练专用计数器导出移到顶层 binary，训练和无 exporter
候选的 `starry_kernel` 编译输入相同；普通 profile-generate 对照的
16887 个记录在训练 ELF 中均能找到，hash/count 无不匹配。
OrangePi-5-Plus-1 上普通 A1 和候选 F1/F2 各为有效的无插桩 full20：
每次 20/20 项、380000/380000 样本、零 `not_parked`。普通 A2 的
OTHER 同核 futex 缺 1 个样本且 `not_parked=1`，整次作废。
F1/F2 的 p50 中位数只有 **10/20** 项达到冻结 Linux RT 的 90%，
最差 OTHER 同核 futex 为 **56.861%**（RT 8458 ns、F 14875 ns）。
此候选已拒绝；三次有效候选启动、同源码全项尾延迟回退门和生产构建门
均未证明，PR 仍为 Draft。训练补丁仅作为实验数据归档，不合入生产源码。

### 1.22 当前源码只读审计与广覆盖 PGO 预检

`resume877-879-checkpoint/` 保存两次只读路径审计和广覆盖 PGO 首次训练
镜像的启动异常。`resume877` 未发现可以证明等价的唤醒事务删减；`resume878`
核对机器码后拒绝改写已由 fat LTO 展开的上下文桥接。`resume879` 首次
将 `someboot` 也插桩，镜像在 early boot 的 profile 计数器 `ldxr`
处同步异常，尚无可用 profile 或无插桩候选，更没有新 full20。
首次镜像已判无效，后续重试另行记账；本次归档不改变上节的 10/20 结论。

### 1.23 预 MMU 排查与五 crate PGO

`resume880-883-five-crate/` 记录两次广覆盖训练的板卡启动失败、预 MMU
插桩调用链审计，以及仅增补 `ax_sync` 的五 crate 原生 PGO。广覆盖
`resume880`/`resume882` 没有可用 profile 或 full20；五 crate 候选的
两次有效、无插桩 full20 为 **11/20** 项达到冻结 Linux RT 90%，
最差 OTHER 同核 futex 为 **57.999%**。这只是被拒绝候选的筛查进展，
第三次有效候选和同源码全项回退门仍未证明，不构成 PR 验收。

## 2. 证据核验

从本目录执行 `sha256sum -c SHA256SUMS` 和 `python3 check.py`，可核对归档的
原始日志、状态、样本完整性及逐项性能和回退；新归档的聚焦诊断
还会核对原始单项日志、有效性与计数器前后差值。新 PMU 诊断分别运行
`python3 resume808-current-pmu/analyze.py`、
`python3 resume809-linux-pmu/analyze.py` 和
`python3 resume810-window-pmu/analyze.py` 复核原始日志与有效性；
后续还应分别运行 `python3 resume811-wake-order/analyze.py` 和
`python3 resume813-stratified-pmu/analyze.py`。`resume812-audit/` 是只读审计，
没有板测日志。它们均不参加 full20 验收。未纳入仓库的
完整镜像哈希保存在状态文件，
不能仅凭日志重建镜像身份。冻结 benchmark SHA256 为
`94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773`。
`check.py` 也会复算 `resume817-819-weighted-pgo` 两次有效 full20
的逐项中位数、90% 门槛和单次普通 A1 对照；它不是三次启动验收。
`resume829-833-path-diagnostics/` 和 `resume834-836-sgi-path/` 的核验
只证明诊断记录完整，不参与 full20 验收。
`resume837-842-local-path/` 运行 `sha256sum -c SHA256SUMS` 与
`python3 resume841-linux-forced-rt/analyze.py` 核对聚焦实验；
这些结果同样不参与 full20 验收。
`resume850-852-policy-axis/` 运行 `sha256sum -c SHA256SUMS` 和
`python3 resume852-settle/check.py` 核对原始日志、二进制哈希、
完整轮次、样本及策略差值；无效的 `resume851` 仅存证。
`resume853-855-fair-yield/resume854-yield-stages/` 运行
`sha256sum -c SHA256SUMS` 与 `python3 check.py` 复核原始阶段计数与
预登记判别；这只是一轮插桩诊断，不参加 full20 验收。
`resume858-ktimer-fresh/` 运行 `sha256sum -c SHA256SUMS --quiet` 与
`python3 check.py` 复核逐轮日志、计数器差值和 generation 配对。
`resume862-futex-wake-stages/` 同样运行 `sha256sum -c SHA256SUMS --quiet`
和 `python3 check.py`；缺席的 17 MB 镜像只能核对记录的 SHA，
本地原件已单独核验。`build.log.gz` 与 `probe-tracked.patch.gz` 是
无时间戳 gzip 原件，解压后 SHA256 分别为
`03857df02e180128b4a0489669062bb10bfc3cb5e99d12d68f18380420ab48bb`
和 `4d71a55ff1a17b3a2c0740c8ba4f358af45ffd453a6c12f04caaa986fb6254ad`。
`build.log.gz` 与 `probe.patch.gz` 是原始文件的无时间戳 gzip 归档；
解压后 SHA256 分别为
`51f90c64df300a70b9f7c3c0fd96753388eb7fb2e88c926784d6ccd56fe4473c`
和 `23ba81b82f825af7af4a704465ea01bc69fc26b48b8b264709e4925b23cd0c10`。
构建镜像没有纳入 Git，脚本只能核对日志中的镜像 SHA 记录，不能重算
缺席的 17 MB 镜像本体哈希；本地原始归档另已核验该镜像。
`resume872-current-pgo-audit/` 可在该目录运行 `sha256sum -c SHA256SUMS`
核对构建门禁输出；它没有镜像或性能日志。
`resume873-current-four-crate/` 归档最新源码训练镜像的临时 exporter
补丁、选择性 wrapper、配置、构建日志和 ELF 派生的计数布局；该目录
同样可运行 `sha256sum -c SHA256SUMS`。训练镜像和 ELF 仅保存在本地，
其 SHA 在该目录的 `README.md` 中，尚无板卡计数或性能验收。

`resume873-876-current-pgo/` 在子目录运行 `sha256sum -c SHA256SUMS`
和 `python3 check.py` 核对原始日志、样本完整性与逐项比例；镜像、ELF、
计数器及 profdata 未纳入 Git，其身份只能核对归档的 SHA256 记录。
`resume877-879-checkpoint/` 在子目录运行 `sha256sum -c SHA256SUMS`
核对首次失败构建与串口原件；镜像哈希保存在该目录说明中，镜像未入 Git。
`resume880-883-five-crate/` 在子目录运行 `sha256sum -c SHA256SUMS`，
并运行 `python3 five-crate/analyze_full20.py` 复核 20 项原始日志与
冻结基线；它只证明两次筛查结果，不覆盖最终验收门禁。
