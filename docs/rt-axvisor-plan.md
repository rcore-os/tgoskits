# 实时 AxVisor 三任务总体规划

> 平台基线：**QEMU aarch64**　RTOS 基线：**Zephyr**
> 状态：规划阶段（未动手实现）。本文件作为 living doc 持续更新。

## 0. 总览与依赖关系

三个任务**串行依赖**，不要并行展开：

```
任务一 实时性改造 + 多核Linux客户机 + Zephyr基线
        │  (产出两个稳定运行的客户机 + 实时性数据)
        ▼
任务二 Linux客户机 ↔ Zephyr客户机 IP链路 + 应用层协议
        │  (产出可靠的双向通信通道)
        ▼
任务三 Linux跑AI推理 → 经任务二协议驱动Zephyr控制 → 闭环 + 端到端延迟
```

每个任务交付：**分支名 / 镜像 / 配置 toml / 构建+启动命令 / 测试脚本 / 结果原始数据**。

## 1. 仓库现状盘点（基线能力 vs 待建）

| 能力 | 现状 | 位置 |
|---|---|---|
| vCPU→pCPU 绑定 | ✅ 配置即可 | VM toml `phys_cpu_ids` |
| 多核 Linux 客户机 | ✅ 现成配置 | `os/axvisor/configs/vms/*/linux-smp2.toml` |
| Zephyr 客户机 | ✅ 现成配置 | `os/axvisor/configs/vms/qemu/aarch64/zephyr-smp1.toml` |
| vCPU = ArceOS task，可调度/抢占 | ✅ | `os/axvisor/src/vmm/vcpus.rs` + `components/axsched`（fifo/rr/cfs） |
| 抢占/IRQ 临界区控制 | ✅ | `ax_kernel_guard::NoPreemptIrqSave`（`vmm/mod.rs:149`） |
| 定时器 | ✅ | `os/axvisor/src/vmm/timer.rs` + `components/timer_list` |
| 内存/设备映射/中断路由 | ✅ 配置即可 | toml `memory_regions` / `passthrough_devices` / `interrupt_mode` |
| 宿主 IP 协议栈（smoltcp 系） | ✅ | `net/ax-net`（tcp/udp/router/dhcp/vsock） |
| **客户机间虚拟网卡 + 软件交换机** | ❌ **待建（任务二核心缺口）** | 当前 `emu_devices=[]`，中断走 passthrough |
| `ivc` inter-VM 模块 | ⚠️ 近乎空壳 | `os/axvisor/src/vmm/ivc.rs` |

## 2. 里程碑与时间线

> 时间为小团队（1–2 人）粗估，单位“周”，按需调整。

### 阶段 0 · 环境与基线（M0，✅ 已完成 2026-06-29）
- **M0.1 ✅** 环境就绪：QEMU 10.2.1、工具链 nightly-2026-05-28、dtc。
- **M0.2 ✅** 单核 Linux 引导到 `/bin/sh`（ext4 根挂载、PF_INET/INET6/PACKET 注册）。
- **M0.3 ✅** 双核 Linux：`SMP: Total of 2 processors activated.` + `CPU1: Booted secondary processor`。
- **M0.4 ✅** Zephyr 引导：`*** Booting Zephyr OS build b70c045e4cca ***`，镜像自带线程操作延迟基准（cycles/ns）。
- **交付**：日志在 `tmp/m0-logs/`，生成配置在 `tmp/vmconfigs/`。

#### M0 复现步骤（QEMU aarch64）
```bash
# 0) 每个 cargo xtask 都需要（brew 的 pkg-config 遮蔽了系统 libudev.pc）
export PKG_CONFIG_PATH=/usr/lib/x86_64-linux-gnu/pkgconfig:$PKG_CONFIG_PATH

# 1) 拉取 guest image bundle（含 linux/zephyr/freertos），解压到 tmp/axbuild/rootfs/qemu-aarch64/
cargo xtask image pull qemu-aarch64

# 2) 由模板生成 patch 过的 vmconfig：kernel_path→绝对路径, image_location→memory
#    模板在 os/axvisor/configs/vms/qemu/aarch64/{linux-smp1,zephyr-smp1}.toml
#    生成物在 tmp/vmconfigs/*.generated.toml（多核：cpu_num=2, phys_cpu_ids=[0,1]）

# 3) 运行（vmconfig 经 AXVISOR_VM_CONFIGS 环境变量在 build.rs 编入二进制 → 换配置会触发重建）
cargo xtask axvisor qemu --arch aarch64 --vmconfigs tmp/vmconfigs/linux-aarch64-qemu-smp1.generated.toml
cargo xtask axvisor qemu --arch aarch64 --vmconfigs tmp/vmconfigs/linux-aarch64-qemu-smp2.generated.toml
cargo xtask axvisor qemu --arch aarch64 --vmconfigs tmp/vmconfigs/zephyr-aarch64-qemu-smp1.generated.toml
```

#### M0 踩坑与发现
- **libudev/pkg-config**：linuxbrew 的 `pkg-config` 默认搜索路径不含系统 `/usr/lib/x86_64-linux-gnu/pkgconfig`，导致 `cargo xtask` 编译 `libudev-sys` 失败。加 `PKG_CONFIG_PATH` 即可，无需 sudo。
- **镜像名与 setup_qemu.sh 不符**：monorepo 用 `cargo xtask image pull qemu-aarch64`（顶层 image 子命令），不是脚本里的 `cargo axvisor image pull qemu_aarch64_linux`（那是独立 axvisor 仓库的写法，本仓库 `cargo axvisor` 无 image 子命令）。
- **Zephyr entry_point 是错的（待修）**：出厂 `configs/vms/qemu/aarch64/zephyr-smp1.toml` 的 `entry_point = 0x4000_10b4`，但 `zephyr-qemu` 是 ARM64 Image（0x38 魔数 `ARM\x64`，首指令 `b #0x10a4`），必须从镜像基址 `0x4000_0000` 进入。用 0x10b4 会静默 fault（VM boot success 但 guest 零输出）。改 `entry_point = 0x4000_0000` 后正常。**Zephyr-on-QEMU-aarch64 不在仓库 CI 验证范围**，可考虑提 PR 修正该配置。
- **vCPU 亲和性可见**：日志 `VCpu task Task(14,"VM[1]-VCpu[0]") created cpumask: [0, ]` —— 任务一调度/亲和性改造的直接观测点。

### 阶段 1 · 任务一 实时性改造与验证（M1–M4，约 4–6 周）
- **M1 测量先行（无改造基线）— ✅ Zephyr 部分完成 2026-06-29，报告见 `docs/realtime/M1-zephyr-baseline.md`**
  - ✅ Zephyr 基线：用预编译 `latency_measure` 基准多次启动采样（N=12），空载 vs 压力对照。关键结论：空载稳定（context-switch ~600–1100ns，jitter <130ns），**压力下 worst-case max 膨胀最高 ~89×**（`heap.malloc` 单次最坏 125µs）。改造靶点 = 压低压力下 max/jitter。
  - ⏳ 未完成：本机无 Zephyr 构建链，未做周期任务抖动 app；hypervisor 侧打点（vCPU 调度/IRQ 注入延迟，vGIC=`virtualization/arm_vgic`）拟挪到 M3；多核 Linux 客户机周期抖动基线待补。
  - ⚠️ 前提：QEMU TCG 非实时环境，绝对值不可作 RT 保证，仅用于相对对比（改造前后、空载/压力）；真实硬件留待板卡阶段。
- **M2 改造：调度与亲和性 — 设计分析（2026-06-29）**
  - **代码现状**（已核实）：vCPU 是 ArceOS task，在 `virtualization/axvm/src/runtime/vcpus.rs::alloc_vcpu_task` 创建，按 `phys_cpu_set` 设 `set_cpumask`（亲和性）但**不设优先级**。调度器是 **FIFO 合作式非抢占**（`components/axsched/src/fifo.rs`：`task_tick` 返回 false，tick 不重调度）——vCPU 一旦运行不会被抢占，直到它自己让出（WFI/阻塞）。`--vmconfigs` 是 `Vec`，支持多 VM。
  - **关键洞察（重定义实验）**：M1 的宿主级燃烧器压力扰动的是 **QEMU/TCG 线程层**，AxVisor 管不到 → 调度改造在该压力模型下**无法体现效果**。要展示 M2 价值，争抢必须发生在 **hypervisor 内**：多 VM 的 vCPU 共享同一 AxVisor pCPU。
  - **正确的 M2 实验**：① baseline = RT VM（Zephyr/Linux）vCPU 与噪声 VM（忙循环 Linux）vCPU 同钉 pCPU 0，合作式 FIFO 下噪声 vCPU 长时间不让出 → RT 抖动；② M2 = partition（噪声 VM 移到 pCPU 1，RT vCPU 独占 pCPU 0）→ RT 抖动回落。
  - **两条实现路径**：
    - **路径 A（配置级 partition，低成本）**：保证 RT vCPU 独占一个 pCPU（分配互不重叠的 `phys_cpu_ids`），并确保宿主后台 task 不落在 RT pCPU。合作式 FIFO + 独占 pCPU 即可获得确定性。多为配置/分配逻辑，少量代码。
    - **路径 B（抢占式 RT 优先级，高成本）**：合作式 FIFO 无法在共享 pCPU 上让高优先级 RT vCPU 抢占噪声 vCPU。需引入**抢占式优先级调度**（启用/实现 round_robin 或优先级 scheduler + `set_priority`），改动 `axsched`/`axtask`。仅当无法给 RT vCPU 独占 pCPU 时才需要。
  - 切入点：`virtualization/axvm/src/runtime/vcpus.rs`（task 创建/亲和性/优先级）+ `components/axsched` + `os/arceos/modules/axtask`。
  - 对照基线：M1 的 Zephyr 基准 + A1 Linux cyclictest（`docs/realtime/M1-zephyr-baseline.md`），但压力须换成**多 VM 同核 co-location**（A3）。
  - **实验探路结果（2026-06-29）**：试图用单 VM `phys_cpu_ids=[0,0]`（两 vCPU 同钉 pCPU 0）制造争抢——**失败**：guest 只识别 1 个 CPU（`SMP: Total of 1 processors activated`），AxVisor 把两 vCPU 折叠成单核，反而 max 延迟更低（576µs vs partition [0,1] 的 2831µs，省了 SMP IPI 开销）。**结论：真正的 in-hypervisor 争抢必须用两个独立 VM 同钉一个 pCPU**，单 VM 自折叠无效。数据 `tmp/m1/data/linux-cyclictest-colocated.txt`。
  - **下一步阻塞点/决策**：要展示 M2，需先搭"噪声 VM + RT VM 同 pCPU"的 co-location harness。已构建裸机 aarch64 自旋 payload（`tmp/m2/noisy/`，naked `_start` @ 0x4000_0000，busy+WFI），但发现：
    - **WFI 让出需定时器唤醒**——裸机 payload 无定时器配置时，一次 WFI 后永久睡死，不再"噪"。
    - **纯忙循环（不让出）在合作式 FIFO 下永不调度走 → 完全饿死同核 RT VM**（RT 连引导都完成不了）。
  - **M2 根本结论（架构层）**：**合作式 FIFO 下，同核 co-location 无法优雅时分共享**（要么饿死、要么需定时器中断驱动让出）。因此：
    - **路径 A（partition，推荐且当前唯一低成本可行）**：RT vCPU **独占 pCPU**，绝不与其他 vCPU/宿主 task 共核。M2 的代码工作 = 在 `alloc_vcpu_task` / 配置层**强制**这一隔离（RT pCPU 排除其他 task），并加配置标记。无需 co-location 测量即可验证（RT VM 在独占 pCPU 上跑出稳定低抖动）。
    - **路径 B（抢占式 RT 优先级调度器）**：仅当必须共享 pCPU。需在 `axsched`/`axtask` 引入 timer-tick 驱动的抢占 + 优先级（现 FIFO `task_tick` 恒 false），改动大。
  - **建议**：M2 走路径 A——用 A1 的 cyclictest 在"RT VM 独占 pCPU 0 + 其余 VM/任务隔离到其他核"配置下测稳定性，对比当前默认（无隔离保证）。已构建工具：`tmp/m2/noisy/`（裸机噪声 payload，备路径 B 或带定时器的 co-location 用）。

#### M2 路径 A 实现记录（2026-06-29，已编译/clippy/fmt 通过）
新增 VM 配置项 `[base] dedicated_cpus = true`：标记该 VM 的 pCPU 为**独占**，其他 VM 的 vCPU task 不再被排到这些 pCPU 上（合作式 FIFO 下即获得无争抢的专核）。改动 6 文件 +76/-3：
- `virtualization/axvmconfig/src/lib.rs`：`VMBaseConfig` 加 `#[serde(default)] pub dedicated_cpus: bool`（toml 可选，默认 false）。
- `virtualization/axvmconfig/src/templates.rs`：模板构造补字段。
- `virtualization/axvm/src/config.rs`：`PhysCpuList` 加 `dedicated` 字段 + `new()` 参数 + `dedicated()` 访问器。
- `os/axvisor/src/config.rs`：`PhysCpuList::new(...)` 传入 `cfg.base.dedicated_cpus`。
- `virtualization/axvm/src/vm.rs`：`AxVM::cpus_dedicated()` 访问器。
- `virtualization/axvm/src/runtime/vcpus.rs`：新增 `dedicated_pcpu_mask()`（枚举所有 dedicated VM 的 pCPU 并集）；`alloc_vcpu_task` 中对**非 dedicated** VM 的 vCPU 从 cpumask 剔除保留 pCPU（`mask & !reserved`，若会清空则保留原值并 warn）。
- **验证**：`cargo xtask axvisor build --arch aarch64` 通过；`cargo xtask clippy -p axvm -p axvmconfig`/`-p axvisor` 全绿；`cargo fmt` 已做。`dedicated_cpus=true` 的多核 Linux cyclictest **正常引导 + 运行**（avg 84µs/max 3.6ms，与 A1 空载同量级，单 VM 下行为不变 → 向后兼容安全）。
- **细化（已实现，重新构建/clippy/fmt 通过）**：enforcement 现也覆盖**未固定 vCPU**——非 dedicated VM 的 `phys_cpu_set()==None` 的 vCPU 被约束到 `enabled & !reserved`（否则它本可跑到 RT 专核上，是真实漏洞）；无 dedicated VM 时保持原行为（不设 cpumask）。改后 6 文件 +93/-4。
- **跨 VM 运行时演示尝试（受 AxVisor/aarch64 限制，未取得干净证据）**：
  - aarch64 FDT parser **要求 `phys_cpu_ids`**，未固定 VM 直接被拒（`phys_cpu_ids is missing` → 创建失败被跳过）；FDT 还会覆盖 toml `phys_cpu_sets`。→ aarch64 单 vcpu VM mask 恒单 bit，pruning 退化为"全有或全无"：重叠只能 WARN（单钉 vcpu 挪不走），不重叠则无操作；**多 bit pruning 分支无法自然触发**。
  - 2-VM（VM1 dedicated 钉 pCPU0 + VM2 裸机噪声钉 pCPU0）：两 VM 均创建成功，但 **VM[2] 在 `Updating FDT memory` 后 setup 挂住**（VM[1] 此处会继续到 SPI/vgic）。疑为裸机 VM 空 passthrough/无 vgic 的 2-VM setup 缺陷，**与 enforcement 逻辑无关**（enforcement 在 vcpu 分配阶段、挂起点之后执行）。
- **M2 结论**：路径 A 代码已落地、编译/clippy/fmt 通过、单 VM 验证安全无回归、未固定漏洞已堵。跨 VM 运行时证据受限于 ① QEMU TCG 不反映真实 RT ② aarch64 FDT 强约束 phys_cpu_ids ③ AxVisor 裸机 2-VM setup 挂起 → **定量 partition 收益应在真实板卡 + 两个正常 guest 上演示**。代码未提交、未开 PR。
- **M3 改造：中断 / 定时器 / 锁临界区**
  - 中断：量化 passthrough 注入延迟，减少 trap-and-emulate；评估 direct injection。
  - 定时器：`vmm/timer.rs` 路径优化，降低定时器→vCPU 唤醒延迟。
  - 锁：`cargo xtask sync-lint --since <ref>` 找可疑 Relaxed/长临界区；缩短 `kspin`/`SpinNoIrq` 持锁。
  - 后台任务：降优先级/关闭非必要宿主 task。
- **M4 验证流程固化**
  - 自动化脚本：周期任务抖动、调度延迟、中断响应延迟、最大延迟、长稳（≥数小时）。
  - **对比矩阵**：改造前 vs 后 × 空载 vs 压力。压力源：`stress-ng` / `hackbench` / `fio`（CPU/内存/IO/网络）跑在另一 Linux 客户机或宿主。
  - **RTOS 基线对照**：裸 Zephyr 跑同样周期任务，说明虚拟化额外开销。
- **交付**：分支、改造点说明、测试脚本、原始数据、对比图表、设计文档 `docs/realtime/`。

### 阶段 2 · 任务二 客户机间通信（M5–M8，约 5–7 周，最难）
- **M5 网络底座设计（先定方案，再动手）**
  - 选定方案 **A：模拟 virtio-net + 软件 L2 交换机**（评审最认可、QEMU/板卡一致复现）。
  - 设备模型注册入口：`virtualization/axdevice/src/{factory.rs,device.rs}`。
  - 交换机连接两个客户机网卡，宿主侧可复用 `net/ax-net`。
  - 备选 B（双 NIC passthrough + 外部桥）/ C（QEMU tap）仅作降级预案。
- **M6 virtio-net 设备模型 + 软件交换机实现**
  - 两个客户机各看到一张 virtio-net，AxVisor 内部转发。
  - 客户机内配置：Linux 拿到 eth、Zephyr 拿到 net iface（lwIP）。
  - 验收：两端互 ping 通 + ARP 正常（仅作连通性自检，不作为最终结果）。
- **M7 应用层协议设计与实现**
  - 协议字段（任务硬性要求）：**版本 / 消息类型 / 载荷长度 / 序号或时间戳 / 错误码或校验**。
  - 支持：控制指令、状态回传、错误通知。
  - 传输二选一：
    - UDP → 自实现 ACK + 超时重传 + 乱序/重复去重。
    - TCP → 消息分帧（length-prefixed）+ 连接超时 + 断连重连 + 异常恢复。
  - 两端程序：Linux 标准 socket；Zephyr BSD socket + lwIP。
- **M8 自动化可靠性 + 性能测试**
  - 跑真实请求/响应流程（自定义 RPC 或本协议），**禁止仅用 ping/iperf/nc/裸 socket** 作为结果。
  - 指标：请求成功率、应用层错误、超时、异常恢复、请求-响应延迟、有效应用吞吐。
  - 文档说明：拓扑、MAC/IP、路由、端口、桥接/NAT、防火墙/访问控制。
- **交付**：分支、协议设计文档、两端程序、测试脚本与数据、网络拓扑文档 `docs/ivc/`。
- **vsock 边界**：默认不计入主通道；如用仅作辅助调试，需在设计文档明确边界。

### 阶段 3 · 任务三 AI + 控制联动（M9–M11，约 4–5 周）
- **M9 Linux 侧 AI 推理**
  - 框架选 **TFLite / ONNX Runtime / ncnn** 之一（无 GPU，选轻量模型：小 CNN 分类 / 简单回归）。
  - 在 Starry/Linux 客户机内跑通推理，输出量化结果。
- **M10 跨客户机闭环**
  - Linux 采集输入 → 推理 → 经任务二协议发 Zephyr → Zephyr 调整控制参数（如 PID 目标值/增益）→ 执行可观察动作（日志 / 虚拟外设 / PWM 占空比数值）→ 状态回传。
  - 被控对象建议：虚拟温控或倒立摆数值仿真（有反馈、易量化）。
- **M11 测量与演示**
  - 端到端延迟：**同侧往返测量**（Linux 发→Zephyr 处理→回传→Linux 记 RTT），说明测量方法、误差来源、精度范围。
  - 可量化演示：基线 = 固定参数手动控制；对照 = AI 参与。≥2 项指标（响应延迟 / 控制误差 / 稳定时间 / 识别准确率 选其二）。
- **交付**：分支、AI 应用、控制程序、延迟测量脚本、演示场景脚本与对比数据、文档 `docs/ai-loop/`。

## 3. 关键技术决策（已定 / 待定）

| 决策 | 选择 | 备注 |
|---|---|---|
| 主平台 | QEMU aarch64 | 唯一齐备 linux-smp2 + zephyr + freertos 配置 |
| RTOS | Zephyr | 自带 BSD socket + lwIP，任务二/三省事 |
| 网络底座 | virtio-net + 软件交换机（方案 A） | 待 M5 详细设计确认 |
| 调度策略 | partition（RT vCPU 独占 pCPU + 固定优先级） | 待 M2 实现确认 |
| 传输层 | TCP vs UDP | 待 M7 定，影响可靠性机制实现 |
| AI 框架 | TFLite / ONNX / ncnn 之一 | 待 M9 定，看客户机依赖可得性 |

## 4. 风险登记

| # | 风险 | 影响 | 缓解 |
|---|---|---|---|
| R1 | **客户机间 virtio-net + 软交换机是从零工程**，工作量最大、不确定性最高 | 阻塞任务二/三 | M5 先出详细设计；预留降级方案 B/C；提前在 M0 后并行预研 |
| R2 | Zephyr 在 AxVisor aarch64 下网络栈/lwIP 适配踩坑 | 阻塞任务二 | M0.4 先确认 Zephyr 网络 iface 能起；必要时回退 FreeRTOS+手配 lwIP 比较成本 |
| R3 | 实时改造（partition 调度）可能与现有 vCPU 调度/IPI 假设冲突（`with_vm_and_vcpu_on_pcpu` 的 cross-CPU dispatch 当前未实现，见 `vmm/mod.rs:166`） | 改造范围扩大 | M2 先读懂 vcpus.rs 全流程；小步改 + 每步 `cargo xtask clippy` |
| R4 | QEMU 上无真实外设，AI→控制动作“可观察性”偏弱 | 任务三说服力 | 用数值仿真被控对象 + 明确指标量化，弥补无物理执行器 |
| R5 | 跨客户机时钟不同源，端到端延迟测量有偏差 | 任务三数据可信度 | 采用同侧往返 RTT 测量，规避时钟同步问题 |
| R6 | 长稳测试（数小时）耗时长，迭代慢 | 进度 | 日常用短时回归，长稳仅在里程碑节点跑 |
| R7 | 改造引入 clippy/fmt 不合规、Cargo.lock 冲突 | CI 失败 | 遵守 AGENTS.md：改后跑 `cargo xtask clippy --package <crate>` + `cargo fmt`；冲突时重生成 Cargo.lock |

## 5. 工程纪律（来自 AGENTS.md / CLAUDE.md）

- 构建/运行/测试一律走 `cargo xtask`，不用裸 `cargo build/run/test`。
- 改逻辑后跑相关 `cargo xtask clippy`，**禁止用 `allow` 掩盖警告**，修根因。
- 改后 `cargo fmt`。
- PR 标题英文 Conventional Commits（`type(scope): content`），正文中文；说明问题/改动/每步逻辑。
- 不加 AI/agent 署名或品牌。
- 改架构启动/SMP/平台逻辑时，同步更新 `.claude/skills/arch-platform-porting/SKILL.md`。

## 6. 相关 skill（动手时按需调用）

- `arch-platform-porting` — 实时改造涉及 axcpu/调度/平台 bring-up 时。
- `cross-kernel-driver` — 实现 virtio-net 设备模型（四层模型 + mmio-api/dma-api）时。
- `starry-test-suit` / `arceos-test-adapter` — 把验证用例接入 xtask 测试体系时。
- `review-single-pr` — 每个任务出 PR 时的自检清单。
