# Axvisor 开发指南

Axvisor 是运行在 ArceOS 基础能力之上的 Type-1 Hypervisor。与 ArceOS / StarryOS 不同，Axvisor 的开发必须同时关注代码、板级配置、VM 配置和 Guest 镜像。本文档覆盖开发环境、Hypervisor 运行时开发、虚拟设备开发、vCPU 管理、VM 与板级配置、Guest 支持、测试策略和调试技巧。

> 架构分层、运行时模块和核心设计机制见 [Axvisor 架构](/docs/architecture/axvisor)。
> 最短命令和快速启动见 [快速开始](/docs/quickstart/overview)。
> 构建系统总览见 [构建与运行](/docs/build/overview)。

---

## 1. 开发环境

### 1.1 工具链

Axvisor 共享 TGOSKits 工作区统一工具链（`nightly-2026-07-15`）。Axvisor 的交叉编译配置位于 `os/axvisor/.cargo/config.toml`，包含各架构的链接器标志和 runner 配置。

### 1.2 QEMU

Axvisor 开发依赖 QEMU 的硬件虚拟化支持：

| 架构 | QEMU 包名 | 虚拟化特性 |
|------|-----------|-----------|
| aarch64 | `qemu-system-aarch64` | EL2 虚拟化扩展 |
| riscv64 | `qemu-system-riscv64` | H 扩展 |
| x86_64 | `qemu-system-x86_64` | VMX 或 SVM |
| loongarch64 | `qemu-system-loongarch64` | 虚拟化支持 |

推荐 QEMU 版本 ≥ 10.2.1。

### 1.3 Guest 镜像准备

Axvisor 支持加载多种 Guest OS 镜像。首次运行前需要通过 `setup_qemu.sh` 准备：

```bash
cargo xtask axvisor defconfig qemu-aarch64
(cd os/axvisor && ./scripts/setup_qemu.sh arceos)
```

该脚本完成以下操作：

1. 从 `axvisor-guest` GitHub 仓库下载 Guest 镜像到 `/tmp/.axvisor-images/qemu_aarch64_arceos`
2. 从 `configs/vms/qemu/aarch64/arceos-smp1.toml` 生成 `os/axvisor/tmp/vmconfigs/arceos-aarch64-qemu-smp1.generated.toml`
3. 自动修正 VM 配置中的 `kernel_path`
4. 复制 `rootfs.img` 到 `os/axvisor/tmp/rootfs.img`

支持的 Guest 镜像类型：`arceos`, `arceos-riscv64`, `linux`, `nimbos`。

---

## 2. 目录结构总览

```
os/axvisor/
├── src/                    # Hypervisor 运行时
│   ├── main.rs             # 入口与 AxVM runtime 启动
│   ├── config.rs           # VM TOML、镜像、machine plan 与创建事务
│   ├── manager.rs          # VM manager 初始化与启动
│   ├── platform_irq.rs     # 平台 IRQ adapter 注册
│   ├── shell/              # 交互式 VM 管理
│   └── banner.rs           # 启动 banner
├── configs/
│   ├── board/              # 板级配置（10 个）
│   │   ├── qemu-aarch64.toml
│   │   ├── qemu-riscv64.toml
│   │   ├── qemu-x86_64.toml
│   │   ├── qemu-loongarch64.toml
│   │   ├── orangepi-5-plus.toml
│   │   ├── phytiumpi.toml
│   │   ├── rdk-s100.toml
│   │   ├── roc-rk3568-pc.toml
│   │   └── tac-e400.toml
│   └── vms/                # VM 配置（50+ 个）
│       ├── linux-*-*.toml
│       ├── arceos-*-*.toml
│       ├── freertos-*-*.toml
│       ├── nimbos-*-*.toml
│       ├── rt-thread-*-*.toml
│       └── zephyr-*-*.toml
└── scripts/
    └── setup_qemu.sh       # QEMU Guest 镜像准备脚本
```

核心组件（位于 `virtualization/`）：

| 组件 | 职责 |
|------|------|
| `axvm` | VM 抽象：`AxVM`, `AxVMRef`, `VMMemoryRegion`, `VMStatus` |
| `axvm-types` + `axvm/src/vcpu.rs` | vCPU 协议与 wrapper：`VmArchVcpuOps`, `VmExit` / `VmExit`，状态机管理 |
| `axdevice` | 两阶段虚拟设备、bus registry 与 interrupt topology |
| `axvisor_api` | Hypervisor API 接口 |
| `axaddrspace` | 地址空间管理 |

---

## 3. Hypervisor 运行时开发

### 3.1 启动流程

Axvisor 的启动流程（`src/main.rs`）：

```
main()
  → 打印 logo
  → 检查硬件虚拟化支持 (has_hardware_support)
  → 初始化 AxVM runtime 与平台 IRQ adapter
  → manager::init()
    → 严格解析 VM 配置
    → 捕获 host platform snapshot
    → 生成 VmMachinePlan 和 FDT/ACPI
    → 加载 Guest 镜像
    → claim 物理设备并事务创建 AxVM
  → manager::start()
    → 启动已提交 VM
    → 进入控制台 shell
```

### 3.2 VMM 核心模块

| 模块 | 文件 | 职责 |
|------|------|------|
| 配置编排 | `os/axvisor/src/config.rs` | 严格解析请求、加载镜像并启动 VM 创建事务 |
| 机型规划 | `virtualization/axvm/src/machine/` | snapshot、ownership、资源分配和固件生成 |
| VM 生命周期 | `virtualization/axvm/src/vm/` | RAM、vCPU、设备、mapping 与 boot state |
| vCPU 运行时 | `virtualization/axvm/src/runtime/` | vCPU task、VM exit 和调度动作 |
| 设备与拓扑 | `virtualization/axdevice/` | 两阶段设备模型、bus 和 interrupt topology |
| 架构适配 | `virtualization/axvm/src/arch/<arch>/` | controller、firmware 与硬件 backend |

### 3.3 修改运行时

| 改动类型 | 位置 | 第一步验证 |
|----------|------|-----------|
| 启动流程 | `src/main.rs` | `cargo xtask axvisor build --config os/axvisor/.build.toml` |
| VMM 逻辑 | `src/config.rs`、`src/manager.rs` | 先 build-only，准备好 Guest 后再 QEMU |
| VM 领域 | `virtualization/axvm/src/` | `cargo test -p axvm --lib --tests` |
| 架构相关 | `virtualization/axvm/src/arch/` | 对应架构 build 与 QEMU 单独验证 |
| Shell | `src/shell/` | 启动后交互测试 |

---

## 4. 虚拟设备开发

### 4.1 设备模型

设备声明与设备创建分成两个阶段。`VirtualDeviceModel::requirements()` 先声明具名 MMIO、
PIO、IRQ、MSI 和 DMA 需求；`VmMachinePlanner` 分配资源后，`build()` 只消费
`ResolvedDeviceResources`。设备不能自己挑选 Guest 地址或中断号。

### 4.2 设备配置

在 VM 配置文件中的设备配置示例：

```toml
[machine]
mode = "passthrough"
firmware = "auto"
interrupts_passthrough = false

[devices]
disable_defaults = []
deny = [
  { kind = "fdt-path", value = "/soc/gpio@fe740000" },
]

[[devices.virtual]]
id = "console0"
model = "arm-pl011"
source = { kind = "auto" }
backend = { kind = "host-console", rx = "exclusive", tx = "shared" }
```

### 4.3 添加模拟设备

要添加一个新的虚拟设备（如虚拟串口、虚拟块设备），需要：

1. 在可复用 `no_std` crate 中实现设备 core。
2. 在 `axdevice` 中实现 `VirtualDeviceModel`，声明稳定的资源 slot。
3. 在架构 profile/adapter 注册模型和固件描述。
4. 设备通过 `DeviceBuildContext::irq(slot)` 或 `msi(slot)` 获取 endpoint，不接触 vCPU。
5. 添加资源分配、bus、firmware 和 Guest 驱动回归测试。

---

## 5. vCPU 管理

### 5.1 vCPU 状态机

vCPU 的生命周期由 `virtualization/axvm/src/vcpu.rs` 管理：

```
Created → Free → Ready → Running ⇄ Suspended → Halted
```

关键状态转换：

| 转换 | 触发 |
|------|------|
| Created → Free | vCPU 初始化完成 |
| Free → Ready | vCPU 绑定到物理 CPU |
| Ready → Running | 被调度器选中执行 |
| Running → Suspended | VM Exit（异常、中断、I/O） |
| Suspended → Running | VM Entry（恢复执行） |
| Running → Halted | Guest 关机或错误 |

### 5.2 VM Exit 处理

vCPU 进入 Running 后，当发生 VM Exit 时，`VmExit` 描述 VM 层退出原因：

```rust
// 退出原因需要由 VMM 处理
match exit_reason {
    VmExit::ExternalInterrupt => { /* 处理外部中断 */ }
    VmExit::NestedPageFault { .. } => { /* 处理 stage-2/EPT/NPT 违规 */ }
    VmExit::Hypercall { .. } => { /* 处理 HVC/ECALL/VMCALL */ }
    VmExit::MmioRead { .. } => { /* 处理 MMIO 读 */ }
    VmExit::MmioWrite { .. } => { /* 处理 MMIO 写 */ }
    // ...
}
```

### 5.3 per-CPU 虚拟化状态

`virtualization/axvm/src/vcpu.rs` 管理每个物理 CPU 上的虚拟化状态，包括当前运行的 vCPU 绑定和架构 per-CPU 后端入口。

---

## 6. VM 配置

### 6.1 VM 配置文件结构

VM 配置文件位于 `os/axvisor/configs/vms/`，TOML 格式：

```toml
[machine]
mode = "virtual"          # 或 "passthrough"
firmware = "auto"

[base]
id = 1                    # VM ID
name = "linux-qemu"       # VM 名称
cpu_num = 1               # vCPU 数量
phys_cpu_ids = [0]        # 绑定的物理 CPU

[kernel]
entry_point = 0x8020_0000           # 入口地址
image_location = "fs"               # "memory"（嵌入二进制）或 "fs"（从文件系统加载）
kernel_path = "/guest/linux/linux-qemu"  # 内核路径
kernel_load_addr = 0x8020_0000      # 内核加载地址
dtb_load_addr = 0x8000_0000         # DTB 加载地址（aarch64）

[[memory.regions]]
guest_base = 0x8000_0000
size = 0x1000_0000
permissions = "rwx"
backing = { kind = "allocate" }

[devices]
disable_defaults = []
deny = []
```

### 6.2 关键字段说明

| 字段 | 说明 | 常见值 |
|------|------|--------|
| `id` | VM 唯一标识 | 正整数 |
| `cpu_num` | 分配的 vCPU 数 | 1-16 |
| `phys_cpu_ids` | 绑定的物理 CPU 列表 | `[0]`, `[0, 1, 2, 3]` |
| `entry_point` | Guest 入口地址 | 架构相关 |
| `image_location` | 镜像加载方式 | `"fs"` 或 `"memory"` |
| `kernel_path` | 内核文件路径 | Guest 类型相关 |
| `machine.mode` | Guest 平台来源 | `"virtual"` 或 `"passthrough"` |
| `interrupts_passthrough` | 透传 VM 是否直投物理 IRQ | `false`（默认）或 `true` |
| `memory.regions` | 显式 Guest memory 与 backing | `[[memory.regions]]` |

### 6.3 支持的 Guest 类型

| Guest | 配置前缀 | 支持的架构/板 |
|-------|---------|-------------|
| **Linux** | `linux-` | aarch64 (qemu, e2000, orangepi5p, rk3568, rk3588, s100, tac_e400), riscv64-qemu |
| **ArceOS** | `arceos-` | aarch64 (qemu, e2000, orangepi5p, rk3568, s100, tac_e400), riscv64-qemu |
| **FreeRTOS** | `freertos-` | aarch64 (e2000, orangepi5p, qemu, tac_e400) |
| **NimbOS** | `nimbos-` | aarch64-qemu, riscv64-qemu, x86_64-qemu |
| **RT-Thread** | `rtthread-` | aarch64-e2000 |
| **Zephyr** | `zephyr-` | aarch64 (e2000, orangepi5p, qemu, tac_e400) |

---

## 7. 板级配置

### 7.1 板级配置文件

板级配置位于 `os/axvisor/configs/board/`，定义 Hypervisor 本身的编译和运行参数：

```toml
# qemu-aarch64.toml
env = { AX_IP = "10.0.2.15", AX_GW = "10.0.2.2" }
features = ["ax-std/bus-mmio", "fs"]
log = "Info"
vm_configs = []   # 注意：默认为空，需手动指定或通过 setup_qemu.sh 生成
```

### 7.2 已支持的板级配置

**QEMU 虚拟板**：

| 配置 | 架构 | 用途 |
|------|------|------|
| `qemu-aarch64` | aarch64 | 主要开发和测试平台 |
| `qemu-riscv64` | riscv64 | RISC-V 虚拟化验证 |
| `qemu-x86_64` | x86_64 | x86 虚拟化验证 |
| `qemu-loongarch64` | loongarch64 | 龙芯虚拟化验证 |

**物理板**：

| 配置 | SoC | 用途 |
|------|-----|------|
| `orangepi-5-plus` | RK3588S | 开发板测试 |
| `phytiumpi` | 飞腾 | 飞腾平台测试 |
| `rdk-s100` | — | RDK 板测试 |
| `roc-rk3568-pc` | RK3568 | RK3568 开发板 |
| `tac-e400` | — | E400 板测试 |

### 7.3 新增板级支持

1. 创建 `os/axvisor/configs/board/<board>.toml`
2. 在 `platforms/` 下添加对应平台 crate（如需要）
3. 创建对应的 VM 配置 `configs/vms/<board>/<guest>-<variant>.toml`
4. 验证：

```bash
cargo xtask axvisor defconfig <board>
cargo xtask axvisor build --config os/axvisor/.build.toml
```

---

## 8. 第一条成功路径：QEMU AArch64

第一次上手强烈建议从 `qemu-aarch64` 开始。

### 8.1 使用 `setup_qemu.sh`

**不要**直接从 `defconfig → build → qemu` 开始——默认配置中的 `vm_configs` 为空，且 `rootfs.img` 不会自动生成。

```bash
# 步骤 1：生成板级配置
cargo xtask axvisor defconfig qemu-aarch64

# 步骤 2：准备 Guest 镜像和 rootfs
(cd os/axvisor && ./scripts/setup_qemu.sh arceos)

# 步骤 3：启动
cargo xtask axvisor qemu \
  --config os/axvisor/.build.toml \
  --qemu-config .github/workflows/qemu-aarch64.toml \
  --vmconfigs os/axvisor/tmp/vmconfigs/arceos-aarch64-qemu-smp1.generated.toml
```

### 8.2 为什么直接跑会失败

| 问题 | 原因 |
|------|------|
| `vm_configs` 为空 | 板级配置默认不包含 VM 配置，需通过 `setup_qemu.sh` 或手动指定 |
| `rootfs.img` 不存在 | 需手动准备或通过脚本下载 |
| `kernel_path` 错误 | 默认路径指向不存在的位置，`setup_qemu.sh` 会自动修正 |

---

## 9. 测试

### 9.1 测试套件结构

`test-suit/axvisor/normal/`：

| 目录 | 内容 |
|------|------|
| `qemu/` | QEMU 冒烟测试（4 个架构） |
| `board-orangepi-5-plus/` | OrangePi-5-Plus 物理板测试 |
| `board-phytiumpi/` | 飞腾 Pi 物理板测试 |
| `board-rdk-s100/` | RDK-S100 物理板测试 |
| `board-roc-rk3568-pc/` | ROC-RK3568-PC 物理板测试 |

### 9.2 测试配置格式

Axvisor 测试配置与 StarryOS 类似，使用 shell 交互模式：

```toml
# build config
vm_configs = ["os/axvisor/configs/vms/qemu/aarch64/linux-smp1.toml"]
features = ["ax-std/bus-mmio", "fs"]
```

```toml
# runtime config
shell_prefix = "~ #"
shell_init_cmd = "pwd && echo 'guest test pass!'"
success_regex = ["(?m)^guest test pass!\\s*$"]
```

**关键差异**：Axvisor 测试需要指定 `vm_configs` 来加载 Guest。

### 9.3 运行测试

```bash
# QEMU 测试
cargo xtask axvisor test qemu --target aarch64

# 指定架构
cargo xtask axvisor test qemu --target riscv64
```

### 9.4 添加新测试用例

1. 准备 Guest 镜像（或使用已有的）
2. 创建 VM 配置（如需要）
3. 在 `test-suit/axvisor/normal/` 对应目录下创建测试
4. 编写 build config（包含 `vm_configs`）和 runtime config
5. 确认 `shell_prefix` 与 Guest shell 提示符匹配
6. 验证

---

## 10. 调试

### 10.1 先看配置，再看代码

Axvisor 启动失败时，**最常见的问题不是代码编译失败**，而是以下四件事没对齐：

| 检查项 | 验证方法 |
|--------|---------|
| `.build.toml` 是否是当前板级配置 | `cat os/axvisor/.build.toml` |
| `vm_configs` 是否为空 | 检查 build config 中的 `vm_configs` 字段 |
| `kernel_path` 是否真实存在 | `ls os/axvisor/tmp/` 查看镜像文件 |
| 入口地址 / 加载地址 / 内存布局是否匹配 | 检查 VM config 中 `entry_point` 与 `memory_regions` |

### 10.2 排错命令

```bash
# 重新生成板级配置
cargo xtask axvisor defconfig qemu-aarch64

# 查看可用板级配置
cargo xtask axvisor config ls

# 只做构建，排除编译问题
cargo xtask axvisor build --config os/axvisor/.build.toml

# 使用脚本准备镜像和 rootfs
(cd os/axvisor && ./scripts/setup_qemu.sh arceos)

# 明确指定 VM 配置运行
cargo xtask axvisor qemu \
  --config os/axvisor/.build.toml \
  --qemu-config .github/workflows/qemu-aarch64.toml \
  --vmconfigs os/axvisor/tmp/vmconfigs/arceos-aarch64-qemu-smp1.generated.toml
```

### 10.3 GDB 调试 Hypervisor

```bash
# 启动带 GDB server 的 QEMU
cargo xtask axvisor defconfig qemu-aarch64
# 手动启动 QEMU 时加 -s -S 参数
```

在另一个终端：

```bash
aarch64-none-elf-gdb <hypervisor-binary>
(gdb) target remote :1234
(gdb) break vmm::init
(gdb) continue
```

### 10.4 调试 Guest

调试 Guest 内部问题需要在 Guest 镜像中添加调试输出：

- **Linux Guest**：启用 `console=ttyAMA0` 等串口输出
- **ArceOS Guest**：使用 `LOG=debug` 编译 Guest
- **FreeRTOS/Zephyr Guest**：在源码中添加 `printf` / `printk`

如果需要在 Hypervisor 层面观察 Guest 行为，可在 VM Exit 处理代码中添加日志：

```rust
// 在 vmm 的 VM Exit 处理中
info!("VM Exit: reason={:?}, vcpu_id={}", exit_reason, vcpu_id);
```

### 10.5 日志级别

```bash
# 通过 build config 设置
# 在板级配置中修改 log 字段
log = "Debug"   # "Error" | "Warn" | "Info" | "Debug" | "Trace"
```

---

## 11. 物理板开发

### 11.1 从 QEMU 到物理板

将 QEMU 验证通过的改动迁移到物理板时，需要额外关注：

| 方面 | QEMU | 物理板 |
|------|------|--------|
| 中断控制器 | GIC (通用) | SoC 专用 GIC 配置 |
| 设备树 | QEMU 生成 | 板级固定 DTB |
| 内存布局 | 简单连续 | 可能有保留区域 |
| 启动方式 | QEMU 直接加载 | U-Boot 引导 |
| 时钟/电源 | 无需配置 | 需初始化 PMU/Clock |
| 存储设备 | virtio-blk | 真实 eMMC/SD/NVMe |

### 11.2 物理板测试

物理板测试通过 U-Boot 和串口进行：

```bash
# 构建 Axvisor
cargo xtask axvisor defconfig orangepi-5-plus
cargo xtask axvisor build --config os/axvisor/.build.toml

# 通过 board xtask 部署和测试
cargo xtask board <subcommand>
```

物理板测试配置位于 `test-suit/axvisor/normal/board-*`。

---

## 12. 与 ArceOS 的关系

Axvisor 构建在 ArceOS 基础能力之上，改动共享模块时的验证策略：

| 改动位置 | 先验证 | 再验证 |
|----------|--------|--------|
| `virtualization/axvm`、`axvm-types`、`*_vcpu`、`axdevice` | `cargo xtask axvisor build` | 准备好 Guest 后 QEMU 测试 |
| `os/arceos/modules/axhal` | ArceOS helloworld | Axvisor build + QEMU |
| `os/arceos/modules/axtask` | ArceOS helloworld | Axvisor build + QEMU |
| `os/axvisor/src/*` | `cargo xtask axvisor build` | QEMU 测试 |
| `os/axvisor/configs/*` | — | 直接 QEMU / 板级测试 |

---

## 13. 推荐阅读

- [Axvisor 架构](/docs/architecture/axvisor): 五层架构、VMM 启动链、vCPU 任务模型
- [组件开发指南](/docs/development/components): Axvisor 与 ArceOS / StarryOS 的共享依赖
- [构建与运行](/docs/build/overview): xtask、辅助脚本与测试入口边界
- [ArceOS 开发指南](/docs/development/arceos): Axvisor 所依赖的 ArceOS 基础能力
