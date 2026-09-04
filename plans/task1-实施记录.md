# Task 1 实施进度记录

> 智能化工控虚拟化擂台赛 · 任务一：实时性改造与验证
> 主文档：[os/axvisor/doc/task1-realtime.md](../os/axvisor/doc/task1-realtime.md)

---

## 进度总览

| 里程碑 | 状态 | 说明 |
|---|---|---|
| M1.1 linux-smp2 配置 | ✅ | `os/axvisor/configs/vms/qemu/aarch64/linux-smp2.toml` |
| M1.2 RT 域 arceos-rt-smp1 配置 | ✅ | pCPU3 独占分区 |
| M1.3 Zephyr 基线模板 | ✅ | `zephyr-rt-baseline.toml` + smoke 用例 |
| M1.3b RT-Thread QEMU 基线 | ✅ | `rtthread-rt-baseline.toml` + 本地构建脚本 |
| M1.4 裸机抖动测量框架 | ✅ | `test-suit/arceos/rust` feature `rt-latency` |
| M1.5 AxVisor linux-smp2 冒烟测试 | ✅ | `test-suit/axvisor/normal/qemu/linux-smp2/` |
| M1.6 task1 脚本 | ✅ | `os/axvisor/scripts/task1/`、`scripts/task1/run-rt-baseline.sh` |
| M2.1 vCPU 优先级抢占 | ✅ | `vcpu_priorities` + `sched-cfs` |
| M2.2 vGIC/定时器优化 | ✅ | 中断 wake 已加；arch timer 直访（跳过 ctxt switch + CNTKCTL） |
| M2.3 虚拟化 vs 裸机对比报告 | 🚧 | idle + stress pre/post 已采集；stress P99 改善 ~1.9–5.6%，未达 ≥50% |
| M3.1 长稳/stress 矩阵脚本 | ✅ | `run-stress-matrix.sh` + `rt-latency-long` feature |
| M3.2 改造前后对比自动化 | ✅ | `collect-rt-latency-report.sh` |

---

## 2026-07-10 阶段一落地内容

### 配置

- **Linux 2vCPU**：`cpu_num = 2`，`phys_cpu_ids = [1, 2]`，内存 1GiB @ `0x8000_0000`
- **RT ArceOS**：`cpu_num = 1`，`phys_cpu_ids = [3]`，内存 128MiB @ `0x4000_0000`

### 测量

- 新增 `rt-latency` 测试：1ms/10ms 周期，200 样本，输出 mean/P99/max
- 注册到 `cargo xtask arceos test qemu -c rt-latency`

### 验证命令

```bash
# 裸机 RTOS 基线
./scripts/task1/run-rt-baseline.sh

# Linux 2vCPU under AxVisor
cargo xtask axvisor test qemu --arch aarch64 -c linux-smp2

# 混合分区（手动）
cd os/axvisor && ./scripts/task1/setup-qemu-aarch64.sh && ./scripts/task1/run-mixed.sh
```

### 已知限制

1. 宿主 vCPU 调度在阶段二前为 FIFO 基线；当前已启用 `sched-cfs` 与 vCPU nice
2. arch timer 客户机直访（CNTV）已实现（`passthrough_timer` + CNTKCTL）
3. RT-Thread / Zephyr QEMU smoke 已接入；IRQ 延迟与 pre-opt stress 长稳待补

---

## 2026-07-10 阶段二落地内容

### 调度改造

- `axvmconfig::VMBaseConfig::vcpu_priorities`：per-vCPU CFS nice
- `axvm::spawn_vcpu_task`：创建 vCPU 宿主任务后应用优先级
- `axtask::set_task_priority`：支持为任意任务设置 nice
- AxVisor / AxVM 启用 `sched-cfs`（可抢占 CFS）
- `os/axvisor/src/task.rs`：管理任务绑 pCPU0
- 中断 `queue_interrupt` 路径：`wake_task` 目标 vCPU

### 配置

- `linux-smp2.toml`：`vcpu_priorities = [10, 10]`
- `arceos-rt-smp1.toml`：`vcpu_priorities = [-20]`

### 脚本

- `os/axvisor/scripts/task1/build-arceos-rt-guest.sh`：构建 memory 加载的 rt-latency 客户机

---

## 2026-07-10 阶段二续 / 阶段三落地内容

### RT 客户机虚拟化测量

- `rt-latency-guest` / `rt-latency-long` feature：`mode=guest` 与长稳采样（180k）
- 输出增加 `p999_jitter_ns`
- 修复 `build-arceos-rt-guest.sh` 路径，统一安装到 `os/axvisor/images/qemu_aarch64_arceos_rt/`
- AxVisor CI 用例：`test-suit/axvisor/normal/arceos-rt-latency/`（smoke：`VM[2] boot success`）
- Guest 镜像拉取：`cargo xtask image pull qemu-aarch64` → `images/qemu-aarch64/arceos/arceos-qemu`

### 阶段三脚本

| 脚本 | 用途 |
|---|---|
| `scripts/task1/run-rt-guest-baseline.sh` | AxVisor guest rt-latency 短测 |
| `scripts/task1/collect-rt-latency-report.sh` | 裸机 vs guest 对比报告（`plans/task1-reports/`） |
| `scripts/task1/run-stress-matrix.sh` | 30min stress 矩阵操作说明 |

### 验证命令

```bash
# 裸机
./scripts/task1/run-rt-baseline.sh

# AxVisor RT 客户机（单 VM）
cargo xtask axvisor test qemu --arch aarch64 -c arceos-rt-latency

# 裸机 vs guest 对比报告
./scripts/task1/collect-rt-latency-report.sh

# 长稳 / stress 操作指引
./scripts/task1/run-stress-matrix.sh
```

### 阶段二续：arch timer 直访（M2.2）

- `arm_vcpu`：`passthrough_timer` 时不再在 VM exit 保存/恢复 CNTV/CNTP 寄存器
- `CNTKCTL_EL1`：允许 EL0 访问物理/虚拟 counter 与 timer（RTOS 用户态 tick）
- `build-arceos-rt-guest.sh`：改走 `os/arceos` make 裸机构建（修复 musl PIE page fault）

### 阶段二续：passthrough GIC 兼容性（M2.2）

**现象**：自构建 `rust_aarch64.bin` + `arceos-rt-smp1.toml`（passthrough GIC）在 GIC 初始化时 panic：
`GICD ioremap failed: addr=0x8000000 size=0x10000: Invalid MMIO address or size`。

**根因**：`rt-latency` feature 未启用 `ax-std/paging`，`axruntime` 不调用 `ax_mm::init_memory_management()`，`mem_iomap` 返回 `Unsupported`。

**修复**：
- `test-suit/arceos/rust/Cargo.toml`：`rt-latency` 增加 `ax-std/paging`
- 辅助改动：`someboot` FDT 设备 MMIO 登记、`axmm` 内核 direct map、`KERNEL_LOAD_PADDR` 默认 `0x8020_0000`、GIC v3 错误信息

**验证**：
```bash
./os/axvisor/scripts/task1/build-arceos-rt-guest.sh
cd os/axvisor && cargo xtask qemu \
  --config configs/board/qemu-aarch64.toml \
  --qemu-config .github/workflows/qemu-aarch64.toml \
  --vmconfigs configs/vms/qemu/aarch64/arceos-rt-smp1.toml
# 期望：RT_LATENCY mode=guest ... RT_LATENCY_PASS
cargo xtask axvisor test qemu --arch aarch64 -c arceos-rt-latency
```

---

## 2026-07-10 阶段三：第一轮混合 stress 长稳实测

### 新增

- `test-suit/axvisor/stress/mixed-rt-stress-round1/`：Linux+RT 双客户机、CPU stress、`rt-latency-long`（180k 样本）
- `scripts/task1/run-mixed-stress-round1.sh`：idle 对比 + 长稳 + 报告归档一键脚本

### 第一轮结果（`20260710T091425Z`）

| 场景 | period_ms | samples | mean (ns) | P99 (ns) | P999 (ns) | max (ns) |
|---|---:|---:|---:|---:|---:|---:|
| guest + stress | 1 | 180000 | 75908 | 258320 | 446416 | 10147056 |
| guest + stress | 10 | 180000 | 100010 | 309648 | 527568 | 10049008 |

- 裸机 idle（200 样本）：见 `plans/task1-reports/compare-20260710T091348Z.md`
- 完整日志：`plans/task1-reports/mixed-stress-round1-20260710T091425Z.log`
- 报告：`plans/task1-reports/mixed-stress-round1-20260710T091425Z.md`
- 耗时：~34 min（`RT_LATENCY_PASS`）

### 复现

```bash
# 一键（idle 对比 + 长稳 ~35min）
./scripts/task1/run-mixed-stress-round1.sh

# 仅长稳 stress 用例
RT_LATENCY_FEATURES=rt-latency,rt-latency-guest,rt-latency-long \
  os/axvisor/scripts/task1/build-arceos-rt-guest.sh
cargo xtask axvisor test qemu --arch aarch64 -g stress -c mixed-rt-stress-round1
```

---

## 下一步

1. **赛题 ≥50% 改善**：当前 pre/post 仅去掉 `vcpu_priorities` 对比，stress 下 P99 改善不足；需更完整「改造前」基线或额外优化
2. **IRQ 响应延迟**：`irq.rs` 虚拟注入 benchmark
3. **交卷材料**：设计说明 + PR，见 `plans/task1-reports/SUBMISSION-STATUS.md`

---

## 2026-07-10 场景矩阵与 idle guest 短测补齐

### 新增

- `arceos-rt-latency-guest`：单 VM 短测，匹配 `RT_LATENCY_PASS`（自构建 rt-latency bench）
- `arceos-rt-latency-guest-pre-opt`：无 `vcpu_priorities` 的改造前 profile
- `arceos-rt-smp1-pre-opt.toml`
- `scripts/task1/collect-task1-matrix-report.sh`：bare + guest pre/post idle 一键采集
- `run-rt-guest-baseline.sh`：改为构建 guest 镜像并跑短测

### 矩阵报告（`matrix-20260710T102947Z.md`）

| 场景 | 1ms P99 (ns) | 10ms P99 (ns) |
|---|---:|---:|
| bare idle | 309312 | 467952 |
| guest idle pre-opt | 178944 | 369760 |
| guest idle post-opt | 262656 | 364720 |
| guest + stress (post-opt, 180k) | 258320 | 309648 |

```bash
./scripts/task1/collect-task1-matrix-report.sh
```

---

## 2026-07-10 pre-opt stress 长稳与 pre/post 对比

### 新增

- `test-suit/axvisor/stress/mixed-rt-stress-round1-pre-opt/`：RT 使用 `arceos-rt-smp1-pre-opt.toml`（无 `vcpu_priorities`）
- `scripts/task1/run-mixed-stress-pre-opt-round1.sh`：pre-opt 长稳 + 与 post-opt 对比报告

### 结果（`stress-pre-post-20260710T105300Z.md`）

| period_ms | pre-opt P99 | post-opt P99 | 改善 |
|---:|---:|---:|---:|
| 1 | 263312 | 258320 | 1.9% |
| 10 | 327904 | 309648 | 5.6% |

| period_ms | pre-opt P999 | post-opt P999 | 改善 |
|---:|---:|---:|---:|
| 1 | 482448 | 446416 | 7.5% |
| 10 | 578400 | 527568 | 8.8% |

- 耗时 ~34min，`RT_LATENCY_PASS`
- 日志：`mixed-stress-pre-opt-20260710T105300Z.log`
- **结论**：仅去掉 `vcpu_priorities` 的 pre/post 对比，stress 下 P99 改善远未达赛题 ≥50%；idle 短样本波动更大，不能单独作为改造证据

### 解析修复

串口输出可能带 `~ # \x1b[6n` 前缀，且 `period_ms=1` 会误匹配 `period_ms=10`。已修正 `parse_rt_latency_lines` / `metric_field`（`collect-task1-matrix-report.sh`、`run-mixed-stress-*.sh`、`collect-rt-latency-report.sh`）。

### 复现

```bash
./scripts/task1/run-mixed-stress-pre-opt-round1.sh   # ~35min
# 或仅跑用例
cargo xtask axvisor test qemu --arch aarch64 -g stress -c mixed-rt-stress-round1-pre-opt
```

---

## 2026-07-10 RT-Thread RT 基线（QEMU aarch64）

### 落地内容

- `rtthread-rt-baseline.toml` / `rtthread-smp1.toml`：`qemu-virt64-aarch64` BSP，`load/entry=0x4008_0000`，pCPU3 + `vcpu_priorities = [-20]`
- `test-suit/axvisor/normal/rtthread-rt-baseline/`：smoke（`VM[1] boot success` + `Booting RT-Thread`）
- `os/axvisor/scripts/task1/build-rtthread-rt-guest.sh`：克隆 RT-Thread + 下载 `aarch64-none-elf` 工具链 + `scons`
- `scripts/task1/run-rtthread-rt-baseline.sh`：一键 smoke

### 验证

```bash
./scripts/task1/run-rtthread-rt-baseline.sh
# 或先构建镜像
os/axvisor/scripts/task1/build-rtthread-rt-guest.sh
cargo xtask axvisor test qemu --arch aarch64 -c rtthread-rt-baseline
```

**说明**：`qemu-aarch64` 镜像 bundle 暂无预编译 RT-Thread，需本地构建或复用 `images/qemu_aarch64_rtthread/`。

---

## 2026-07-10 Zephyr RT 基线（QEMU aarch64）

### 落地内容

- `zephyr-rt-baseline.toml`：指向 `images/qemu-aarch64/zephyr/zephyr-qemu`，pCPU3 + `vcpu_priorities = [-20]`
- `test-suit/axvisor/normal/zephyr-rt-baseline/`：AxVisor smoke（`VM[1] boot success` + `Booting Zephyr OS`）
- `os/axvisor/scripts/task1/setup-zephyr-rt-baseline.sh`：拉取镜像
- `scripts/task1/run-zephyr-rt-baseline.sh`：一键 smoke

### 验证

```bash
./scripts/task1/run-zephyr-rt-baseline.sh
# 或
cargo xtask axvisor test qemu --arch aarch64 -c zephyr-rt-baseline
```
