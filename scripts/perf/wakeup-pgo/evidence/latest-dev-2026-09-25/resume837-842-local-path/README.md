# 本地唤醒与交接诊断

本目录归档 `dev@05175ca388` 加 PR 中 AArch64 `memset` 修复后的只读审计，
以及 OrangePi-5-Plus-2 上的一组同二进制、跨系统聚焦实验。实验不修改
Starry 调度器，也不构成冻结 benchmark 的 full20 验收。

## 1. 诊断过程

前三项审计分别核对跨核 SGI 后半段、OTHER 同核 futex，以及 OTHER
定时器 worker。它们约束了可尝试的运行时改动范围，没有找到能解释
多微秒缺口且满足现有并发契约的单点优化。

### 1.1 唤醒路径审计

`resume837-post-sgi-audit/` 核对 CPU1 识别 SGI 后的 owner-rq 与切换路径；
`resume838-samecpu-handoff-audit/` 核对私有 futex key、`ThreadWakeBatch`、
`wake_thread_source()` 和接收者 park；`resume839-timer-worker-audit/`
核对 `ParkSoft` 所需的 ktimer worker。各目录的 `decision.md` 给出主代理
复核结论，`subagent/final.txt` 保存原始只读探索报告。旧插桩均值不等于
当前原生镜像的可删除成本。

### 1.2 同板配对实验

`resume840-forced-rt-preempt/` 在未修改的 G 镜像上，把聚焦同核 FIFO
接收者优先级从 80 改为 81，发送者仍为 80；同时运行重编译的等优先级
对照和冻结二进制。`resume841-linux-forced-rt/` 在同一板上启动冻结
Linux PREEMPT_RT，按相同顺序运行**同三枚用户态二进制**。Starry 八行
均为 20000/20000 样本、零 `not_parked` 和 `missed_deadlines`；Linux
八行中七行有效，OTHER 对照第二轮为 19999/20000、`not_parked=1`，
不得用其构造两轮中位数。两系统均未在各测量窗口同步采频；G 镜像和
Linux RT 的约 816 MHz 来自独立频率探针。

有效 FIFO 数据说明改变优先级会同时改善两系统，而 Starry 的相对
差距并未消失。此处的 ratio 定义为 `Linux_RT_p50 / Starry_p50`，并非
冻结 full20 的正式通过率。

| 同核 FIFO 条件 | Starry 两轮 p50 中位数 (ns) | Linux RT 两轮 p50 中位数 (ns) | RT/Starry |
| --- | ---: | ---: | ---: |
| 重编译等优先级对照 | 11958 | 8604.5 | 71.96% |
| 接收者优先级 81 | 10792 | 6562.5 | 60.81% |
| 冻结等优先级二进制 | 12104 | 8458.5 | 69.88% |

这组变体与冻结 full20 的工作负载不同。它只能反驳“差距全由 Fair
策略选择造成”的解释，不能据此提出一个已验证的调度器改动。

## 2. 证据与限制

`resume842-local-immediate-audit/` 逐段检查 `FUTEX_WAKE_PRIVATE` 至
`execute_switch_plan()`、架构切换和接收者 switch tail。`OwnerRqTxn`
交接、`on_cpu` 发布、丢唤醒屏障和运行时间记账均有必要的并发语义；
审计未找到可证明省下约 4 微秒的事务级候选。

### 2.1 原始材料

`resume840` 保存两枚诊断二进制、源码、构建配置、三次会话状态和
原始串口；前两次会话无有效完整诊断组，只有 `run3/` 进入表格。
`resume841` 保存 Linux RT 启动串口、一次性 initramfs、状态和
`analyze.py`。在本目录运行
`python3 resume841-linux-forced-rt/analyze.py` 可逐行复核哈希、次序、
样本完整性以及无效 OTHER 轮次。外部 G 和 Linux 内核镜像未复制到 PR，
其 SHA256 均记录在对应 `decision.md` 与状态文件。运行
`sha256sum -c SHA256SUMS` 可核对本目录归档文件。

### 2.2 验收边界

最新有效、无插桩且频率配置可比的 G1/G2 full20 仍只有 **11/20**
项达到冻结 Linux RT p50 的 90%，最差 OTHER 同核 futex 为 **58.00%**。
本目录没有新 full20、没有可保留运行时优化，也没有完成三次候选启动
或同源码 `<3%` 尾延迟回退门。PR 继续保持 Draft。
