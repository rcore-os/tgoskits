# Axvisor 开发指南

Axvisor 是一个基于 ArceOS 构建的统一模块化 Type I 虚拟机监控器（Hypervisor）。

## 📋 目录

- [简介](#简介)
- [快速开始](#快速开始)
- [架构设计](#架构设计)
- [开发流程](#开发流程)
- [虚拟机管理](#虚拟机管理)
- [设备虚拟化](#设备虚拟化)
- [调试技巧](#调试技巧)
- [进阶主题](#进阶主题)

## 简介

### 特性

- ✅ **统一架构**: 单一代码库支持 x86_64、ARM64、RISC-V
- ✅ **模块化设计**: 功能组件化，易于扩展
- ✅ **多 Guest 支持**: 运行 ArceOS、Linux、StarryOS、NimbOS
- ✅ **硬件虚拟化**: 利用硬件虚拟化扩展
- ✅ **设备虚拟化**: VirtIO 设备支持
- ✅ **多平台**: QEMU、树莓派、飞腾派等

### 架构支持

| 架构 | 状态 | 硬件虚拟化 |
|------|------|------------|
| **ARM64** | ✅ 支持 | ARMv8-A VHE |
| **x86_64** | ✅ 支持 | VT-x / EPT |
| **RISC-V** | ✅ 支持 | H-extension |

### Guest 系统支持

| Guest 系统 | 类型 | 架构支持 | 状态 |
|-----------|------|----------|------|
| **ArceOS** | Unikernel | ARM64, x86_64, RISC-V | ✅ |
| **Linux** | 宏内核 | ARM64, x86_64, RISC-V | ✅ |
| **StarryOS** | 教学OS | ARM64, x86_64 | ✅ |
| **NimbOS** | RTOS | ARM64, x86_64, RISC-V | ✅ |

## 快速开始

### 环境准备

```bash
# 安装基础工具
sudo apt install -y build-essential cmake clang libssl-dev pkg-config

# 安装 Rust 工具链
rustup target add aarch64-unknown-none-softfloat
rustup target add x86_64-unknown-none
rustup target add riscv64gc-unknown-none-elf

# 安装工具
cargo install cargo-binutils
```

### 在 TGOSKits 中构建

```bash
# 1. 进入 Axvisor 目录
cd os/axvisor

# 2. 选择配置（QEMU ARM64）
cargo xtask defconfig qemu-aarch64

# 3. 构建
cargo xtask build

# 4. 运行
cargo xtask run
```

### 快速运行（完整流程）

```bash
# 1. 准备 Guest 镜像（参考 axvisor-guest 仓库）
git clone https://github.com/arceos-hypervisor/axvisor-guest
cd axvisor-guest

# 2. 构建 ArceOS Guest
./build-arceos.sh aarch64

# 3. 回到 Axvisor 目录
cd ../axvisor

# 4. 配置并运行
cargo xtask defconfig qemu-aarch64
cargo xtask build
cargo xtask run
```

### 使用 Makefile

```bash
cd os/axvisor

# ARM64
make ARCH=aarch64 run

# x86_64
make ARCH=x86_64 run

# RISC-V
make ARCH=riscv64 run
```

## 架构设计

### 五层架构

```
┌─────────────────────────────────────────┐
│      Guest VMs (ArceOS/Linux/...)       │  虚拟机
├─────────────────────────────────────────┤
│    Virtual Devices (VirtIO/GIC/...)     │  虚拟设备
├─────────────────────────────────────────┤
│    VM Management (axvm/axvcpu/...)      │  VM 管理
├─────────────────────────────────────────┤
│    CPU Virtualization (vCPU/VMX/...)    │  CPU 虚拟化
├─────────────────────────────────────────┤
│    Hardware (ArceOS HAL + Drivers)      │  硬件层
└─────────────────────────────────────────┘
```

### 核心组件

| 组件 | 路径 | 说明 |
|------|------|------|
| **axvm** | `components/axvm` | 虚拟机管理 |
| **axvcpu** | `components/axvcpu` | vCPU 管理 |
| **axvisor_api** | `components/axvisor_api` | API 接口 |
| **arm_vcpu** | `components/arm_vcpu` | ARM vCPU |
| **arm_vgic** | `components/arm_vgic` | ARM 虚拟中断控制器 |
| **riscv_vcpu** | `components/riscv_vcpu` | RISC-V vCPU |
| **riscv_vplic** | `components/riscv_vplic` | RISC-V 虚拟中断控制器 |
| **x86_vcpu** | `components/x86_vcpu` | x86 vCPU |
| **axdevice** | `components/axdevice` | 设备虚拟化 |
| **axaddrspace** | `components/axaddrspace` | 地址空间管理 |

### 目录结构

```
os/axvisor/
├── src/                  # 主程序
├── configs/             # 配置文件
│   ├── board/           # 开发板配置
│   └── vms/             # VM 配置
├── crates/              # 内部 crates
└── doc/                 # 文档
```

## 开发流程

### 配置系统

#### 1. 开发板配置

配置文件位于 `configs/board/` 目录：

```toml
# configs/board/qemu-aarch64.toml
[build]
arch = "aarch64"
smp = 4
log = "info"

[plat]
name = "axplat-aarch64-qemu-virt"

[features]
default = ["virtio", "gicv3"]

[vm_configs]
# VM 配置列表（需要手动指定）
vms = ["configs/vms/arceos-aarch64.toml"]
```

#### 2. VM 配置

```toml
# configs/vms/arceos-aarch64.toml
[vm]
name = "arceos"
kernel = "path/to/arceos.bin"

[vm.memory]
size = "128M"

[vm.cpus]
num = 2

[vm.devices]
# 设备列表
```

### 构建命令

```bash
# 选择配置
cargo xtask defconfig <board_name>

# 查看当前配置
cargo xtask menuconfig

# 构建
cargo xtask build

# 清理
cargo xtask clean

# 运行
cargo xtask run

# 调试模式运行
cargo xtask debug
```

### 添加新的开发板

1. **创建配置文件**

```toml
# configs/board/my-board.toml
[build]
arch = "aarch64"
smp = 2
log = "debug"

[plat]
name = "axplat-aarch64-myboard"

[features]
default = ["virtio"]

[vm_configs]
vms = ["configs/vms/arceos-aarch64.toml"]
```

2. **实现平台支持**

```rust
// components/axplat_crates/platforms/axplat-aarch64-myboard/src/lib.rs
use axplat::Platform;

pub struct MyBoardPlatform;

impl Platform for MyBoardPlatform {
    fn name() -> &'static str {
        "my-board"
    }
    
    // 实现必要的 trait...
}

axplat::register_platform!(MyBoardPlatform);
```

3. **使用配置**

```bash
cargo xtask defconfig my-board
cargo xtask build
cargo xtask run
```

## 虚拟机管理

### 创建虚拟机

```rust
use axvm::{AxVM, AxVMOptions};

let options = AxVMOptions {
    name: "my-vm",
    memory_size: 128 * 1024 * 1024,  // 128MB
    num_cpus: 2,
    kernel_image: "path/to/kernel",
    ..Default::default()
};

let vm = AxVM::new(options)?;
vm.start()?;
```

### vCPU 管理

```rust
use axvcpu::AxVCpu;

// 创建 vCPU
let vcpu = AxVCpu::new(0)?;

// 设置初始状态
vcpu.set_pc(entry_point);
vcpu.set_reg(0, 0);  // a0 = 0
vcpu.set_reg(1, dtb_addr);  // a1 = DTB address

// 运行 vCPU
vcpu.run()?;

// 处理 VM exit
loop {
    let exit_reason = vcpu.run()?;
    match exit_reason {
        VmExit::ExternalInterrupt => {
            // 处理外部中断
        }
        VmExit::Hypercall => {
            // 处理 hypercall
        }
        _ => {}
    }
}
```

### 内存管理

```rust
use axaddrspace::AddrSpace;

// 创建地址空间
let mut addr_space = AddrSpace::new();

// 映射内存区域
addr_space.map_range(
    guest_phys_addr,
    host_virt_addr,
    size,
    Flags::READ | Flags::WRITE | Flags::EXECUTE,
)?;

// 加载内核
addr_space.load_kernel(kernel_image)?;
```

## 设备虚拟化

### VirtIO 设备

```rust
use axdevice::virtio::{VirtIOBlk, VirtIONet};

// 创建虚拟块设备
let blk = VirtIOBlk::new(0x10000, 128 * 1024 * 1024)?;
vm.add_device(blk)?;

// 创建虚拟网卡
let net = VirtIONet::new(0x20000)?;
vm.add_device(net)?;
```

### 中断控制器

#### ARM - GIC

```rust
use arm_vgic::{GicV3, Vgic};

// 创建虚拟 GIC
let vgic = GicV3::new(gicd_base, gicc_base)?;

// 注入虚拟中断
vgic.inject_irq(vcpu_id, irq_num)?;
```

#### RISC-V - PLIC

```rust
use riscv_vplic::VPlic;

// 创建虚拟 PLIC
let vplic = VPlic::new(plic_base)?;

// 注入虚拟中断
vplic.inject_irq(vcpu_id, irq_num)?;
```

### Pass-through 设备

```rust
// 设备直通（需要硬件支持）
let device = PassthroughDevice::new(pci_address)?;
vm.add_device(device)?;
```

## 调试技巧

### 启用调试日志

```bash
# 方法1：通过配置文件
# configs/board/qemu-aarch64.toml
[build]
log = "debug"

# 方法2：环境变量
export LOG=debug
cargo xtask run
```

### 使用 GDB 调试 Hypervisor

```bash
# 1. 启动 QEMU 并等待 GDB
cargo xtask debug

# 2. 在另一个终端连接 GDB
aarch64-elf-gdb target/aarch64-unknown-none/release/axvisor

# GDB 命令
(gdb) target remote :1234
(gdb) break vm_entry
(gdb) continue
```

### 调试 Guest 内核

```bash
# 1. 启动 Axvisor
cargo xtask run

# 2. 在 Guest 内核编译时包含调试信息
# 3. 使用 GDB 连接到 Guest
(gdb) target remote :1234
(gdb) break rust_main  # Guest 的入口点
```

### QEMU 监控命令

```bash
# 在 QEMU 运行时按 Ctrl+A, C
(qemu) info status         # VM 状态
(qemu) info registers      # 寄存器
(qemu) info mtree          # 内存布局
(qemu) info cpus           # CPU 信息
(qemu) x/10i $pc           # 反汇编
```

## 进阶主题

### 添加新的虚拟设备

1. **实现设备 trait**

```rust
use axdevice::{Device, DeviceIO};

pub struct MyVirtualDevice {
    // 设备状态
}

impl Device for MyVirtualDevice {
    fn name(&self) -> &str {
        "my-device"
    }
}

impl DeviceIO for MyVirtualDevice {
    fn read(&self, offset: usize, size: usize) -> Result<u64> {
        // 实现读取逻辑
        Ok(0)
    }
    
    fn write(&self, offset: usize, value: u64, size: usize) -> Result<()> {
        // 实现写入逻辑
        Ok(())
    }
}
```

2. **注册设备**

```rust
vm.add_device(MyVirtualDevice::new())?;
```

### 实现 Hypercall

```rust
// 定义 hypercall 编号
const HC_MY_HYPERCALL: u64 = 100;

// 处理 hypercall
fn handle_hypercall(vcpu: &AxVCpu, code: u64, args: [u64; 6]) -> Result<u64> {
    match code {
        HC_MY_HYPERCALL => {
            debug!("my_hypercall called: {:?}", args);
            Ok(0)
        }
        _ => Err(Error::UnknownHypercall),
    }
}

// 在 VM exit 处理中调用
match exit_reason {
    VmExit::Hypercall => {
        let code = vcpu.get_reg(0);
        let args = [
            vcpu.get_reg(1),
            vcpu.get_reg(2),
            // ...
        ];
        let ret = handle_hypercall(vcpu, code, args)?;
        vcpu.set_reg(0, ret);
    }
}
```

### 性能优化

1. **使用大页**

```rust
// 使用 2MB 大页
addr_space.map_range(
    guest_addr,
    host_addr,
    size,
    Flags::HUGE_PAGE,
)?;
```

2. **减少 VM exit**

```rust
// 批量处理中断
vgic.enable_batch_mode();
```

3. **优化内存映射**

```rust
// 使用 EPT/NPT 扩展页表
addr_space.enable_ept();
```

### 支持新架构

1. **实现 vCPU 接口**

```rust
// components/newarch_vcpu/src/lib.rs
use axvcpu::AxVCpuOps;

pub struct NewArchVCpu {
    // 架构特定状态
}

impl AxVCpuOps for NewArchVCpu {
    fn run(&mut self) -> Result<VmExit> {
        // 实现运行逻辑
    }
    
    // 实现其他方法...
}
```

2. **实现中断控制器**

```rust
pub struct NewArchInterruptController {
    // 中断控制器状态
}

impl InterruptController for NewArchInterruptController {
    // 实现必要方法...
}
```

## 常见问题

### Q: Guest 启动失败

**A:** 检查以下几点：
1. Guest 镜像格式是否正确
2. 内存配置是否足够
3. 设备树是否正确

```bash
# 查看 Guest 镜像信息
file path/to/guest.bin

# 检查配置
cat configs/vms/arceos-aarch64.toml
```

### Q: 设备虚拟化不工作

**A:** 确认：
1. VirtIO 驱动是否加载
2. 中断是否正确配置
3. 内存映射是否正确

### Q: 如何查看 VM 状态

**A:** 使用 Axvisor shell：

```bash
# 启动后进入 Axvisor shell
> vm list              # 列出 VM
> vm status <id>       # 查看 VM 状态
> vcpu list <vm-id>    # 列出 vCPU
```

## 硬件平台

### QEMU

| 平台 | 架构 | 配置 |
|------|------|------|
| qemu-aarch64 | ARM64 | `configs/board/qemu-aarch64.toml` |
| qemu-x86_64 | x86_64 | `configs/board/qemu-x86_64.toml` |
| qemu-riscv64 | RISC-V | `configs/board/qemu-riscv64.toml` |

### 实际硬件

| 平台 | 架构 | 状态 |
|------|------|------|
| Orange Pi 5 Plus | ARM64 | ✅ |
| Phytium Pi | ARM64 | ✅ |
| ROC-RK3568-PC | ARM64 | ✅ |
| EVM3588 | ARM64 | ✅ |

## 参考资源

- [Axvisor 官方文档](https://arceos-hypervisor.github.io/axvisorbook/)
- [Axvisor 仓库](https://github.com/arceos-hypervisor/axvisor)
- [ARM 虚拟化手册](https://developer.arm.com/documentation/)
- [Intel VT-x 手册](https://www.intel.com/content/www/us/en/developer/articles/technical/intel-virtualization-technology-specifications.html)
- [RISC-V H-extension](https://github.com/riscv/riscv-virtual-memory)

---

**下一步**: 学习 [组件开发指南](components.md) 了解如何开发可复用组件
