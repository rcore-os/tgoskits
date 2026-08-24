# Goal Prompt — 完成任务一（实时性改造）+ 任务二（客户机间通信）

> 自包含目标提示。可直接作为新会话/自主 agent 的初始 prompt。复制 `## GOAL PROMPT` 以下全部内容投喂即可。

---

## GOAL PROMPT

你在 `tgoskits` monorepo（OS/虚拟化工作区）中工作，目标是在 **AxVisor**（基于 ArceOS 的 Type-1 hypervisor）上**完成两个任务**。严格遵守仓库 `AGENTS.md` / `CLAUDE.md` 约定。

### 0. 已确立的事实（不要重新发现）

- **主平台 = QEMU aarch64；RTOS 基线 = Zephyr**。开发主目录 `os/axvisor/`，构建系统 `cargo xtask`。
- **运行机制**（详见记忆 `axvisor-qemu-run-howto` 与 `docs/rt-axvisor-plan.md`）：
  - 每个 `cargo xtask` 前必须 `export PKG_CONFIG_PATH=/usr/lib/x86_64-linux-gnu/pkgconfig:$PKG_CONFIG_PATH`（否则 libudev-sys 编译失败）。
  - 拉客户机镜像：`cargo xtask image pull qemu-aarch64`（一个 bundle，含 `linux/linux-qemu`、`zephyr/zephyr-qemu`、`freertos/`），解压到 `tmp/axbuild/rootfs/qemu-aarch64/`。**不要**用 `os/axvisor/scripts/setup_qemu.sh`（它是独立仓库写法，本仓库 `cargo axvisor image` 无效）。
  - vmconfig 由 `os/axvisor/configs/vms/qemu/aarch64/*.toml` 复制并 patch（`kernel_path`→绝对路径、`image_location`→`memory`；多核 Linux：`cpu_num=2`、`phys_cpu_ids=[0,1]`），放 `tmp/vmconfigs/*.generated.toml`。
  - 运行：`cargo xtask axvisor qemu --arch aarch64 --vmconfigs <generated.toml>`。**vmconfig 经 `AXVISOR_VM_CONFIGS` 环境变量在 build.rs 编入二进制 → 改 vmconfig 会触发增量重建**。停止：`pkill -9 -f 'release/axvisor.bin'`。
  - **已知 bug**：出厂 `zephyr-smp1.toml` 的 `entry_point` 须改为 `0x4000_0000`（`zephyr-qemu` 是 ARM64 Image，必须从基址进入），否则 VM boot success 但 guest 零输出。
  - ⚠️ **QEMU 是 TCG（非 KVM）**：绝对延迟值不代表真实硬件 RT，只能做**相对对比**（改造前后、空载/压力）。
- **已完成**：M0（单核/双核 Linux、Zephyr 均引导成功）；M1（Zephyr `latency_measure` 基准的空载 vs 压力基线，N=12，报告 `docs/realtime/M1-zephyr-baseline.md`，脚本/数据 `tmp/m1/`）。关键发现：空载稳定，**压力下 worst-case max 膨胀最高 ~89×**。
- AxVisor 当前用 ArceOS **FIFO 非抢占调度**，vCPU 是带 cpumask 的 ArceOS task（见 `os/axvisor/src/vmm/vcpus.rs`、`components/axsched`）。

### 1. 任务一 · 实时性改造与验证（验收清单）

- [ ] **改造 AxVisor 提升确定性**：优化调度/抢占/定时器/中断路径/CPU 亲和性/锁临界区/后台任务中的关键路径。切入点：`os/axvisor/src/vmm/{vcpus.rs,timer.rs}`、`components/axsched`、`virtualization/arm_vgic`、`kspin`/`kernel_guard`。核心方向：**partition 调度 + RT vCPU 1:1 独占 pCPU + 固定优先级**；宿主与非 RT 客户机限制在剩余核。不得用 `allow` 掩盖 clippy。
- [ ] **启动 ≥2 vCPU 多核 Linux 客户机**，并在文档说明：vCPU/物理 CPU 绑定、内存分配、设备映射、中断路由、启动参数。
- [ ] **补全 M1 缺失的测量维度**（改造前基线 → 改造后对照）：
  - 周期任务抖动（Linux guest 跑周期任务/cyclictest；Zephyr 侧若装 SDK 则自建 app，否则用现有基准）。
  - 调度延迟、中断响应延迟（建议 **hypervisor 侧打点**：trap 入口→vCPU resume、IRQ 注入）。
  - 最大延迟、长稳（≥数小时）。
  - 给出测试命令、运行时长、CPU 负载分布、结果数据。
- [ ] **压力验证**：CPU/内存/I/O/网络压力下用 `stress-ng`/`hackbench`/`fio` 或等价负载（本机无这些工具，可用可移植等价负载，见 `tmp/m1/run_with_stress.sh`），对比**改造前后**及**空载/压力**下的 worst-case 延迟与抖动。更具代表性的压力模型：**Zephyr + Linux 压力 VM 同 pCPU 争抢**。
- [ ] **RTOS 基线对照**：Zephyr 在相同/等价平台跑同类周期任务与压力测试，说明平台差异对结果的影响。
- [ ] **可复现**：分支、镜像、配置、构建与启动命令、测试脚本、RTOS 基线配置、结果采集方式齐全。

### 2. 任务二 · 客户机间通信（验收清单）

- [ ] **IP 链路**：在 Linux 客户机 ↔ Zephyr 客户机之间建立基于 IP 协议栈的**双向**网络链路（虚拟网卡/桥接/用户态网络/实际网口）。**共享内存、HyperCall、裸 MMIO 门铃不得作主数据通道**。**推荐方案 A**：在 `virtualization/axdevice`（`factory.rs`/`device.rs`）实现 **virtio-net 设备模型 + 软件 L2 交换机**连接两客户机；宿主侧可复用 `net/ax-net`。当前 `emu_devices=[]`、`vmm/ivc.rs` 近乎空壳，是从零工程（项目最大风险 R1，优先攻坚）。
- [ ] **应用层协议**：跑在 TCP/UDP/IP 之上，含 **版本 / 消息类型 / 载荷长度 / 序号或时间戳 / 错误码或校验** 字段，支持控制指令、状态回传、错误通知。两端程序：Linux 标准 socket；Zephyr BSD socket + lwIP。
- [ ] **可靠性机制**：UDP → ACK/超时/重传/乱序/重复去重；TCP → 消息分帧/连接超时/断连重连/异常恢复。
- [ ] **自动化测试**：基于上层应用协议的真实请求/响应流程（HTTP/MQTT/自定义 RPC/本协议）。**不得仅用 ping/iperf/netcat/裸 socket 作为验证结果**。统计：请求成功率、应用层错误、超时、异常恢复、请求-响应延迟、有效应用吞吐。
- [ ] **文档**：网络拓扑、MAC/IP 地址、路由、端口、桥接/NAT 规则、防火墙/访问控制策略。
- [ ] **vsock 边界**：默认不计入主通道；如使用仅作辅助/调试/对比，并在设计文档明确边界。

### 3. 硬性约束（来自 AGENTS.md / CLAUDE.md）

- 一切构建/运行/测试走 `cargo xtask`，不用裸 `cargo build/run/test`。
- 改逻辑后跑 `cargo xtask clippy --package <crate>`；**禁止用 `allow` 掩盖警告，修根因**。改后 `cargo fmt`。
- PR 标题英文 Conventional Commits（`type(scope): content`），正文中文，说明问题/改动/每步逻辑。**不加任何 AI/agent 署名或品牌**。
- 改架构启动/SMP/平台逻辑时同步更新 `.claude/skills/arch-platform-porting/SKILL.md`；做驱动用 `.claude/skills/cross-kernel-driver/SKILL.md` 的四层模型。
- 解决合并冲突时不手动合 `Cargo.lock`，重新生成。

### 4. 工作方式

- 串行推进：任务一 → 任务二（任务二依赖两个稳定运行的客户机）。每完成一个里程碑，更新 `docs/rt-axvisor-plan.md` 与记忆。
- **诚实汇报**：测试失败就说失败并附输出；跳过的步骤要说明；只对真正验证过的结果下"完成"结论。保留 TCG 非实时的前提声明。
- 长操作用后台 + Monitor 监控构建/引导/测试，覆盖成功与失败信号；用完清理残留进程（qemu、压力负载）。
- 每个任务产出可复现交付物：分支、配置、命令、脚本、原始数据、设计/测试文档（`docs/realtime/`、`docs/ivc/`）。

### 5. 当前下一步

从**任务一 M2**起步：改造调度与亲和性（partition + RT vCPU 独占 pCPU + 固定优先级），并在改造前补 hypervisor 侧打点（A2）与多核 Linux 周期抖动基线（A1），用 M1 基线做改造前后对照。
