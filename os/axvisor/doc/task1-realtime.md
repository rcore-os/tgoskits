# Task 1：AxVisor 实时性改造与验证 — 实施指南

> 对应赛题任务一（30 分）与 `plans/技术方案.md` 阶段一～二。  
> 本文档描述**当前已落地**的配置、脚本、测试与后续改造路线。

---

## 1. 目标与阶段划分

| 阶段 | 内容 | 状态 |
|---|---|---|
| **阶段一** | 基线复现、测量框架、linux-smp2、裸机 RTOS 基线 | ✅ 已启动 |
| **阶段二** | vCPU 优先级/抢占、pCPU 独占强化、vGIC/定时器优化 | 🚧 调度已完成；timer 直访已实现 |
| **阶段三** | 30min 长稳、stress 矩阵、改造前后对比报告 | 🚧 idle 矩阵已采集；stress pre-opt 待补 |

---

## 2. pCPU 分区设计（QEMU aarch64, `-smp 4`）

| pCPU | 角色 | 配置文件 |
|---|---|---|
| 0 | AxVisor 宿主（shell、维护任务） | `configs/board/qemu-aarch64.toml` |
| 1–2 | Linux 客户机 2 vCPU（AI / stress 域） | `configs/vms/qemu/aarch64/linux-smp2.toml` |
| 3 | RT 客户机 1 vCPU（实时控制域） | `configs/vms/qemu/aarch64/arceos-rt-smp1.toml` |

**说明**：当前宿主调度为 ArceOS **FIFO 协作调度**（未启用 `sched-rr`），`phys_cpu_ids` 通过 cpumask 约束 vCPU 可运行 pCPU；阶段二将补充优先级抢占与独占 enforcement。

---

## 3. 新增配置与脚本

### 3.1 VM 配置

| 文件 | 用途 |
|---|---|
| `configs/vms/qemu/aarch64/linux-smp2.toml` | Linux 2 vCPU，`phys_cpu_ids = [1, 2]` |
| `configs/vms/qemu/aarch64/arceos-rt-smp1.toml` | RT 域 ArceOS 1 vCPU，`phys_cpu_ids = [3]` |
| `configs/vms/qemu/aarch64/zephyr-rt-baseline.toml` | Zephyr 裸机/客户机基线（image pull） |
| `configs/vms/qemu/aarch64/rtthread-rt-baseline.toml` | RT-Thread 裸机/客户机基线（本地构建） |

### 3.2 一键脚本（在 `os/axvisor/` 下执行）

```bash
# 准备镜像与生成 tmp 配置
./scripts/task1/setup-qemu-aarch64.sh

# 仅启动 Linux 2vCPU 客户机
./scripts/task1/run-linux-smp2.sh

# 启动 Linux + RT 混合分区（任务一目标拓扑）
./scripts/task1/run-mixed.sh
```

### 3.3 裸机 RTOS 抖动基线（M3）

```bash
# 仓库根目录
./scripts/task1/run-rt-baseline.sh
# 等价于
cargo xtask arceos test qemu --arch aarch64 -g rust -c rt-latency
```

输出示例（解析前缀 `RT_LATENCY`）：

```
RT_LATENCY mode=bare period_ms=1 samples=200 mean_jitter_ns=... p99_jitter_ns=... max_jitter_ns=...
RT_LATENCY mode=bare period_ms=10 samples=200 ...
RT_LATENCY_PASS
```

### 3.4 阶段二：vCPU 优先级（已实现）

VM 配置 `[base]` 新增 `vcpu_priorities`（Linux CFS nice，`-20` 最高）：

| 客户机 | 配置 | nice |
|---|---|---|
| `linux-smp2.toml` | `vcpu_priorities = [10, 10]` | 低于默认 |
| `arceos-rt-smp1.toml` | `vcpu_priorities = [-20]` | 实时域最高 |

宿主启用 `sched-cfs`；`build_vcpu_task` 经 `spawn_vcpu_task` 在创建后调用 `set_task_priority`。
中断入队路径对目标 vCPU 任务额外 `wake_task`，减少 Halt 等待延迟。

构建 RT 客户机 `rt-latency` 镜像：

```bash
cd os/axvisor && ./scripts/task1/build-arceos-rt-guest.sh
```

客户机模式输出 `mode=guest`；长稳采样使用 `rt-latency-long`（约 30min @ 10ms 周期）。

### 3.5 阶段三：对比与 stress 脚本

```bash
# 仓库根目录
./scripts/task1/run-rt-guest-baseline.sh          # AxVisor guest idle 短测 (RT_LATENCY_PASS)
./scripts/task1/collect-rt-latency-report.sh      # 裸机 vs guest idle 简版
./scripts/task1/collect-task1-matrix-report.sh    # bare + guest pre/post idle 矩阵
./scripts/task1/run-stress-matrix.sh              # 30min stress 操作说明
```

报告输出目录：`plans/task1-reports/`。

### 3.6 改造前后 stress 对比（2026-08 增补）

| 脚本 | 基线 profile | 说明 |
|---|---|---|
| `run-stress-baseline-vs-opt-long.sh` | emulated timer + 无 priorities | **最佳证据**，180k 样本 |
| `run-stress-strong-baseline-vs-opt-short.sh` | pCPU2 共核 + nice=19 | 8× CPU stress |
| `run-stress-contended-baseline-vs-opt-short.sh` | pCPU3 与 Linux vCPU1 共核 | 含 `task1-baseline-slow-vtimer`（见下） |
| `run-stress-host-share-baseline-vs-opt-short.sh` | pCPU0 与 AxVisor 宿主共核 | 最弱拓扑尝试 |
| `run-stress-contended-baseline-vs-opt-long.sh` | contended 长稳 180k | ~70min |

**注意**：`rt-latency` guest 使用 `thread::sleep`，不走 CNTP_TVAL 模拟路径；`task1-baseline-slow-vtimer` 对当前 P99 指标无效。

实板（OrangePi-5-Plus / RK3588）VM 配置见 `configs/vms/orangepi-5-plus/*rt*`。

**实板镜像与测试：**

```bash
# 1. 构建 RT guest flat 镜像
./os/axvisor/scripts/task1/build-arceos-rt-guest-board.sh

# 2. 部署到板子 Linux rootfs（需 BOARD_IP）
./scripts/task1/deploy-board-rt-guest.sh <board-ip>

# 3. 快速 smoke（200 样本，无 stress 环）
SKIP_DEPLOY=1 ./scripts/task1/run-board-rt-smoke.sh
cargo xtask axvisor test board --board orangepi-5-plus-linux -c board-orangepi-5-plus-mixed-rt-smoke

# 4. stress 对比（18000 样本，需 self-hosted / board lease）
CARGO_TARGET_DIR=$PWD/target ./scripts/task1/run-board-stress-baseline-vs-opt.sh
```

Test-suit 用例：
- `test-suit/axvisor/normal/board-orangepi-5-plus-mixed-rt-smoke/`
- `test-suit/axvisor/stress/board-orangepi-5-plus-mixed-rt-stress-baseline-short/`
- `test-suit/axvisor/stress/board-orangepi-5-plus-mixed-rt-stress-round1-opt-short/`

---

## 4. 自动化测试

| 测试 | 命令 | 验收 |
|---|---|---|
| 裸机 RT 抖动基线 | `cargo xtask arceos test qemu --arch aarch64 -g rust -c rt-latency` | 输出 `RT_LATENCY_PASS` |
| AxVisor RT 客户机冒烟 | `cargo xtask axvisor test qemu --arch aarch64 -c arceos-rt-latency` | `VM[2] boot success`（pulled `qemu-aarch64/arceos/arceos-qemu`） |
| AxVisor RT 客户机抖动（手动） | 混合分区 + `build-arceos-rt-guest.sh` bare-metal 镜像就绪后 | 输出 `RT_LATENCY_PASS`（`mode=guest`） |
| Linux 2vCPU 冒烟 | `cargo xtask axvisor test qemu --arch aarch64 -c linux-smp2` | shell 输出 `linux-smp2 pass`（`nproc` = 2） |

测试用例路径：

- `test-suit/arceos/rust/src/task/rt_latency.rs`
- `test-suit/axvisor/normal/arceos-rt-latency/`
- `test-suit/axvisor/normal/linux-smp2/`

---

## 5. 实时性测量方法

### 5.1 周期任务抖动（已实现）

- **负载**：1ms / 10ms 周期 `sleep_until` 唤醒
- **指标**：mean / P99 / max jitter（纳秒）
- **时间源**：ArceOS monotonic clock（`Instant`）
- **裸机基线**：`rt-latency` 测试 feature
- **虚拟化基线**：阶段二在相同负载下于 AxVisor Guest 内复跑并对比

### 5.2 中断响应延迟（计划）

- 参考 `test-suit/arceos/rust/src/task/irq.rs`
- 阶段二增加 GPIO/虚拟 timer 注入 → handler 首指令延迟统计

### 5.3 长稳与 stress（计划）

```bash
# 压力源：Linux 客户机内
stress-ng --cpu 2 --vm 1 --fork 4 --timeout 1800s

# 采样：RT 客户机持续输出 RT_LATENCY 行，host 侧重定向到 CSV
```

---

## 6. 裸机 / 原生 RTOS 基线

| RTOS | QEMU aarch64 | 状态 |
|---|---|---|
| ArceOS `rt-latency` | `cargo xtask test arceos -c rt-latency` | ✅ 已实现 |
| Zephyr | `zephyr-rt-baseline.toml` + `cargo xtask image pull qemu-aarch64` | ✅ smoke 已接入 |
| RT-Thread | `rtthread-rt-baseline.toml` + `build-rtthread-rt-guest.sh` | ✅ smoke 已接入 |

**平台差异说明**：QEMU `virt` 与实板（RK3588/RK3568）在 GIC 版本、定时器精度、CPU 频率上存在差异；正式报告需分平台列出数据并说明不可直接横向对比的条件。

---

## 7. 阶段二改造清单（待 PR）

| 改造项 | 涉及路径 | 保底方案 |
|---|---|---|
| vCPU 静态优先级 | `axvmconfig`、`axvm/runtime/vcpus.rs`、`axtask` | ✅ `vcpu_priorities` + CFS nice |
| 启用可抢占调度 | `os/axvisor/Cargo.toml`、`axvm/Cargo.toml` → `sched-cfs` | ✅ 已启用 |
| vGIC 注入路径优化 | `virtualization/arm_vgic/` | ⚠️ 保持现有；已加 vCPU wake |
| passthrough GIC guest ioremap | `test-suit/arceos/rust`（`rt-latency` + `paging`） | ✅ 自构建 guest 可完成 GIC 初始化 |
| arch timer 直访 | `virtualization/arm_vcpu/` | ✅ passthrough_timer 跳过 timer ctxt switch + CNTKCTL |
| Hypervisor 后台任务绑核 | `os/axvisor/src/task.rs` | ✅ pCPU0 管理域 |

---

## 8. 复现检查清单

- [ ] `cargo xtask axvisor defconfig qemu-aarch64`
- [ ] `./scripts/task1/setup-qemu-aarch64.sh`
- [ ] `cargo xtask arceos test qemu --arch aarch64 -g rust -c rt-latency`
- [ ] `cargo xtask axvisor test qemu --arch aarch64 -c linux-smp2`
- [ ] `./scripts/task1/run-mixed.sh`（手动确认双客户机串口）

---

## 9. 相关文档

- 赛题任务说明：`plans/技术方案.md` §3.1
- 环境问题：`plans/开发环境问题记录.md`
- AxVisor FDT 配置：`os/axvisor/doc/FDT_Configuration_Guide.md`
