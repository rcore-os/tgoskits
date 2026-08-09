# Axvisor 开发指南

Axvisor 是运行在 ArceOS 基础能力之上的 Type-1 Hypervisor。与 ArceOS / StarryOS 不同，Axvisor 的开发必须同时关注代码、板级配置、VM 配置和 Guest 镜像。本文档覆盖开发环境、Hypervisor 运行时开发、虚拟设备开发、vCPU 管理、VM 与板级配置、Guest 支持、测试策略和调试技巧。

> 架构分层、运行时模块和核心设计机制见 [Axvisor 架构](/docs/architecture/axvisor)。
> 最短命令和快速启动见 [快速开始](/docs/quickstart/overview)。
> 构建系统总览见 [构建与运行](/docs/build/overview)。

---

## 1. 开发环境

### 1.1 工具链

Axvisor 共享 TGOSKits 工作区统一工具链（`nightly-2026-07-15`）。维护中的构建路径由工作区级 Cargo 配置和 `cargo xtask axvisor` 统一管理；`os/axvisor/.cargo/config.toml` 保留从 Axvisor 目录直接执行 Cargo 时所需的 release、链接和 runner 配置。

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

Axvisor 的维护用例由 `cargo xtask axvisor test` 根据 `test-suit/axvisor/` 中的配置准备 Guest 镜像、rootfs、VM 配置和运行参数。可先列出或直接运行 smoke 用例：

```bash
cargo xtask axvisor test qemu --list --arch aarch64
cargo xtask axvisor test qemu --arch aarch64 --test-group normal --test-case smoke
```

维护中的 Guest 类型包括 ArceOS 和 Linux；具体组合以测试套件中的用例为准。

---

## 2. 目录结构总览

```
os/axvisor/
├── src/                    # Hypervisor 运行时
│   ├── main.rs             # 入口：打印 logo → 检查硬件虚拟化 → vmm::init() → vmm::start()
│   ├── hal/                # 硬件抽象层
│   │   ├── mod.rs          # AxMmHalImpl（地址空间内存分配），架构分发
│   │   └── arch/           # 架构相关虚拟化原语
│   │       ├── aarch64/
│   │       ├── loongarch64/
│   │       ├── riscv64/
│   │       └── x86_64/
│   ├── vmm/                # 虚拟机管理器
│   │   ├── mod.rs          # VMM init/start，VM 启动，VM 列表管理
│   │   ├── config.rs       # VM 配置加载（文件系统或静态）
│   │   ├── vcpus.rs        # vCPU 设置和管理
│   │   ├── vm_list.rs      # 全局 VM 列表
│   │   ├── images/         # Guest 镜像加载
│   │   ├── fdt/            # 为 Guest 生成设备树
│   │   ├── timer.rs        # 虚拟定时器管理
│   │   ├── hvc.rs          # Hypervisor Call 处理
│   │   └── ivc.rs          # 跨 VM 通信
│   ├── shell/              # 交互式控制台（VM 管理）
│   ├── task.rs             # vCPU 任务扩展 trait
│   └── logo.rs             # ASCII art logo
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
│       ├── rt-thread-*-*.toml
│       └── zephyr-*-*.toml
```

核心组件（位于 `components/`）：

| 组件 | 职责 |
|------|------|
| `axvm` | VM 抽象：`AxVM`, `AxVMRef`, `VMMemoryRegion`, `VMStatus` |
| `axvm-types` + `axvm/src/vcpu.rs` | vCPU 协议与 wrapper：`VmArchVcpuOps`, `VmExit` / `VmExit`，状态机管理 |
| `axdevice` | 虚拟设备框架：passthrough / emulated / excluded |
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
  → 启用虚拟化
  → vmm::init()
    → 加载 VM 配置
    → 创建 AxVM 实例
    → 加载 Guest 镜像
    → 初始化 vCPU
    → 生成设备树（如需要）
  → vmm::start()
    → 启动所有 VM
    → 进入控制台 shell
```

### 3.2 VMM 核心模块

| 模块 | 文件 | 职责 |
|------|------|------|
| 配置加载 | `vmm/config.rs` | 从文件系统或静态配置加载 VM 定义 |
| vCPU 管理 | `vmm/vcpus.rs` | vCPU 创建、初始化和调度 |
| VM 列表 | `vmm/vm_list.rs` | 全局 VM 注册表 |
| 镜像加载 | `vmm/images/` | 将 Guest kernel/initramfs/DTB 加载到 VM 内存 |
| 设备树 | `vmm/fdt/` | 为 Guest 生成设备树（描述内存、设备等） |
| 定时器 | `vmm/timer.rs` | 虚拟定时器，为 Guest 提供时间服务 |
| HVC | `vmm/hvc.rs` | 处理 Guest 的 Hypervisor Call |
| IVC | `vmm/ivc.rs` | 跨 VM 通信机制 |

### 3.3 修改运行时

| 改动类型 | 位置 | 第一步验证 |
|----------|------|-----------|
| 启动流程 | `src/main.rs` | `cargo xtask axvisor build --config os/axvisor/.build.toml` |
| VMM 逻辑 | `src/vmm/` | 先 build-only，准备好 Guest 后再 QEMU |
| HAL | `src/hal/` | build + 对应架构 QEMU 测试 |
| 架构相关 | `src/hal/arch/` | 只影响对应架构，需单独验证 |
| Shell | `src/shell/` | 启动后交互测试 |

---

## 4. 虚拟设备开发

### 4.1 设备模型

Axvisor 把用户物理设备策略与 machine 固有虚拟设备分开：

| 层次 | 配置/实现 | 说明 |
|------|-----------|------|
| **物理设备选择** | `devices.passthrough` | `virtualized` 客户机只直通显式选择的设备 |
| **默认直通排除** | `devices.disabled` | 从 `passthrough` 客户机的默认可分配设备集中移除 |
| **虚拟平台设备** | `axvm::machine` | 固定创建串口、中断控制器、定时器和固件接口，不进入用户配置 |

### 4.2 设备配置

在 VM 配置文件中的设备配置示例：

```toml
[base]
guest_type = "passthrough"

[devices]
passthrough = [{ path = "/soc/ethernet@1000" }]
disabled = [{ path = "/soc/gpio@2000" }]
```

`guest_type = "passthrough"` 已表示默认选择全部 guest-assignable 物理设备，不需要也不允许 `"/"` 通配选择器。宿主物理 UART 始终不可分配；客户机串口始终是 machine 创建的虚拟设备。

### 4.3 添加模拟设备

要添加一个新的虚拟设备（如虚拟串口、虚拟块设备），需要：

1. 在 `virtualization/axdevice/` 中实现设备模拟逻辑
2. 在 `virtualization/axvm/src/machine.rs` 的对应架构 profile 中注册固定资源
3. 通过 `IrqLine` 接入对应虚拟中断控制器，并在 VM Exit 路径分发设备访问
4. 同步生成 FDT/ACPI/MP table 描述并通过 Guest 驱动验证

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
[base]
id = 1                    # VM ID
name = "linux-qemu"       # VM 名称
guest_type = "virtualized" # "virtualized" 或 "passthrough"
cpu_num = 1               # vCPU 数量
phys_cpu_ids = [0]        # 绑定的物理 CPU

[kernel]
entry_point = 0x8020_0000           # 入口地址
image_location = "fs"               # "memory"（嵌入二进制）或 "fs"（从文件系统加载）
kernel_path = "/guest/linux/linux-qemu"  # 内核路径
kernel_load_addr = 0x8020_0000      # 内核加载地址
dtb_load_addr = 0x8000_0000         # DTB 加载地址（aarch64）

memory_regions = [
  # [base_addr, size, flags, map_type]
  [0x8000_0000, 0x1000_0000, 0x7, 1],
]

[devices]
passthrough = [{ path = "/soc/ethernet@1000" }]
disabled = [{ path = "/soc/gpio@2000" }]
```

### 6.2 关键字段说明

| 字段 | 说明 | 常见值 |
|------|------|--------|
| `id` | VM 唯一标识 | 正整数 |
| `guest_type` | 物理设备赋予策略 | `"virtualized"` 或 `"passthrough"` |
| `cpu_num` | 分配的 vCPU 数 | 1-16 |
| `phys_cpu_ids` | 绑定的物理 CPU 列表 | `[0]`, `[0, 1, 2, 3]` |
| `entry_point` | Guest 入口地址 | 架构相关 |
| `image_location` | 镜像加载方式 | `"fs"` 或 `"memory"` |
| `kernel_path` | 内核文件路径 | Guest 类型相关 |
| `memory_regions` | 内存区域 | `[[base, size, flags, map_type]]` |

配置使用 `deny_unknown_fields`；普通虚拟设备使用 `[[devices.virtual]]` 的 `id + model + options`，地址与中断由解析后设备图分配。默认串口 ID 为 `console0`，可按同 ID 覆盖型号和语义参数，或用新 ID 增加串口；顶层 `serial`、裸地址/IRQ 和 `enabled = false` 仍会失败。旧 `vm_type`、`emu_devices`、`interrupt_mode` 与 `kernel.disk_path` 同样不兼容。完整语义见 [Axvisor 客户机配置与 Machine 设备模型](/docs/architecture/axvisor-guest-machine)。

### 6.3 支持的 Guest 类型

| Guest | 配置前缀 | 支持的架构/板 |
|-------|---------|-------------|
| **Linux** | `linux-` | aarch64 (qemu, e2000, orangepi5p, rk3568, rk3588, s100, tac_e400), riscv64-qemu |
| **ArceOS** | `arceos-` | aarch64 (qemu, e2000, orangepi5p, rk3568, s100, tac_e400), riscv64-qemu |
| **FreeRTOS** | `freertos-` | aarch64 (e2000, orangepi5p, qemu, tac_e400) |
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
vm_configs = []   # 注意：默认为空，需由测试用例或命令行显式指定
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

### 8.1 使用维护中的测试入口

测试入口会根据用例声明准备构建配置、Guest 镜像、rootfs、VM 配置和 QEMU 参数：

```bash
cargo xtask axvisor test qemu \
  --arch aarch64 \
  --test-group normal \
  --test-case smoke
```

### 8.2 为什么手工拼接参数容易失败

| 问题 | 原因 |
|------|------|
| `vm_configs` 为空 | 板级配置默认不包含 VM 配置，应由测试用例或显式参数指定 |
| `rootfs.img` 不存在 | 需通过镜像管理命令或测试入口准备 |
| `kernel_path` 错误 | VM 配置中的镜像路径必须与本地镜像存储一致 |

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

# 列出并运行维护中的 QEMU 用例
cargo xtask axvisor test qemu --list --arch aarch64
cargo xtask axvisor test qemu --arch aarch64 --test-group normal --test-case smoke
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
| 宿主根存储 | NVMe | RK3588 DWCMSHC eMMC |

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
