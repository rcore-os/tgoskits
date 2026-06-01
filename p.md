# Axvisor 架构调整方案

## 1. 背景与目的

当前 ArceOS 对外存在多类能力入口：

- `ax-api`：ArceOS 原生公共 API，覆盖 console、fs、task、time、display 等能力。
- `ax-std`：类 Rust `std` 的应用侧 facade，例如 `fs`、`io`、`thread`、`sync`、`time`。
- `ax-api::modules`：对底层 ArceOS crate 的 re-export，例如 `ax_hal`、`ax_task`、`ax_alloc`、`ax_fs`。
- 底层 ArceOS crate：例如 `ax_hal`、`ax_task`、`ax_alloc`。
- `arceos-rust`：面向 Rust `std` 兼容的过渡实现，不是最终 `target_os = "arceos"` 的 native std。

普通 ArceOS 应用通常只需要：

```rust
ax_std::{fs, io, thread, sync, time, net}
```

Axvisor 不是普通应用。它除了配置、日志、shell、文件读写等普通业务能力外，还需要 CPU、IRQ、页表、frame/连续页/DMA、per-cpu 虚拟化状态、vCPU task、timer、硬件虚拟化开关、架构 EL2/VMX/SVM/H/LVZ 等底层能力。为了避免这些能力在业务层、VM runtime、虚拟化组件之间混杂，本方案按职责重新划分边界。

从 `dev` 基线看，Axvisor 对 ArceOS 能力的访问并不统一，主要存在三类路径。

第一类是通过 `ax_std::os::arceos::api` 使用 `ax-api`：

```rust
extern crate ax_std as std;

use std::os::arceos::api::time::ax_wall_time;
use std::os::arceos::api::task::{AxCpuMask, ax_set_current_affinity};
```

第二类是通过 `ax_std::os::arceos::modules` 间接使用 `ax-api::modules` 暴露的底层模块：

```rust
extern crate ax_std as std;

use std::os::arceos::modules::ax_fs;
use std::os::arceos::modules::ax_hal;
use std::os::arceos::modules::ax_task::{TaskExt, TaskInner};
```

这条路径本质上是：

```text
ax_std::os::arceos::modules
    -> ax-api::modules
        -> ax_hal / ax_task / ax_alloc / ax_fs / ...
```

第三类是直接依赖底层 ArceOS crate：

```rust
use ax_hal;
use ax_hal::{dtb, mem};
use ax_hal::time::busy_wait;
use ax_task::{AxTaskRef, TaskInner, WaitQueue};
```

也就是说，原有 Axvisor 并不是通过一个稳定边界访问 ArceOS host 能力，而是混合使用 `ax-std`、`ax-api::modules` 和底层 ArceOS crate。同时，部分 `virtualization/*` 组件又通过 `axvisor_api` 间接请求 host memory/time/vmm/arch/console 等能力，进一步拉宽了业务层、VM runtime、host adapter、虚拟化组件之间的耦合面。

目标边界如下：

```text
vCPU / irqchip / virtual device / address space 等组件
    只实现虚拟化能力和领域抽象
    不访问 OS 能力
    如需 host 能力，通过组件本地 HostIf 由 axvm 注入

axvm
    表示单 VM 抽象和 VM runtime 原语
    组合 vCPU/device/irqchip/address-space
    定义内部 Host trait
    通过私有 ArceOS host adapter 使用底层 host 能力
    不处理客户机配置文件扫描、镜像加载策略、shell 业务

os/axvisor
    作为最上层 hypervisor 管理程序
    加载客户机配置
    处理客户机镜像和 FDT
    创建、注册、启动、停止 VM
    提供管理 shell
    普通输出、文件读写、环境访问使用 std-like API
```

最终希望达到：

- 删除 `axvisor_api` / `axvisor_api_proc`。
- 除 `axvm` 这个 ArceOS-backed runtime 适配层外，底层 `virtualization/*` capability 组件保持 `no_std`、OS 无关，只依赖 `core`、`alloc`、基础类型和组件本地 host interface。
- `axvm` 不暴露可由外部构造的 ArceOS host adapter，不让外部直接构造 `ArceOsHost` 或访问 `host::arceos`。
- `axvm` 可以保留少量受控 helper，服务 FDT boot 信息、host filesystem release、guest paging handler 等 Axvisor 顶层业务需要。
- `os/axvisor` 不直接调用 `ax_std::os::arceos::{api, modules}`、`ax_hal`、`ax_task`、`ax_alloc`。
- 客户机配置文件、镜像读取、FDT 生成、默认 VM 集合管理都归 `os/axvisor`。
- VM 生命周期原语、vCPU task、VM exit 分发、timer/IRQ glue 归 `axvm`。
- 为后续 ArceOS native std 支持预留边界：业务层从 `ax_std::{fs, io, thread, ...}` 迁移到真正 `std::{fs, io, thread, ...}`。

## 2. 设计方案

### 2.1 总体架构

目标依赖关系：

```text
os/axvisor
    -> axvm
    -> axvmconfig
    -> axvm-types
    -> ax_std::{fs, io, println, thread, ...}

axvm
    -> axvcpu
    -> arm_vcpu / x86_vcpu / riscv_vcpu / loongarch_vcpu
    -> axaddrspace
    -> axdevice_base / axdevice
    -> arm_vgic / x86_vlapic / riscv_vplic
    -> axvm-types
    -> internal Host trait
    -> private ArceOS host adapter

axvm/src/host/arceos.rs
    -> ax_std::os::arceos::{api, modules}

virtualization capability components, excluding axvm
    -> 不依赖 os/axvisor
    -> 不依赖 ArceOS
    -> 不依赖 ax_std
    -> 不依赖 axvisor_api
    -> 通过组件本地 *HostIf 请求必要 host 能力
```

启动路径：

```text
os/axvisor/src/main.rs
    -> AxvmManager::new()
        -> AxvmRuntime::new()
            -> axvm private ArceOS host adapter
            -> 检查并启用硬件虚拟化
    -> AxvmManager::init_default_vms()
        -> os/axvisor::config 解析客户机配置
        -> os/axvisor::images 加载客户机镜像
        -> os/axvisor::fdt 生成客户机 FDT
        -> axvm::register_vm()
        -> AxvmRuntime::init_vms()
        -> 必要时释放 host filesystem 给 x86 guest passthrough
    -> AxvmManager::start_default_vms()
        -> AxvmRuntime 启动已注册 VM
    -> shell::console_init()
```

`os/axvisor` 的业务入口保持薄：

```rust
fn main() {
    print_logo();

    let manager = manager::AxvmManager::new()
        .expect("failed to initialize AxVM manager");

    manager.init_default_vms();
    manager.start_default_vms();

    shell::console_init();
}
```

业务层不允许显式访问：

```rust
ArceOsHost::new()
axvm::host::arceos
ax_std::os::arceos::{api, modules}
ax_hal
ax_task
ax_alloc
```

业务层允许通过 `axvm` 或 `AxvmManager` 暴露的受控 helper 完成少量 host 相关业务动作，例如：

```rust
axvm::shutdown_host_filesystems()
axvm::host_fdt_bootarg()
axvm::host_phys_to_virt(...)
```

### 2.2 `os/axvisor`：最上层管理程序

`os/axvisor` 是产品层和业务编排层，不是底层 host adapter。它负责把用户配置和运行策略转成 `axvm` 能理解的 VM 对象与运行命令。

当前目录职责：

```text
os/axvisor/src/
├── main.rs                         # 启动入口，只串联 manager 和 shell
├── manager.rs                      # AxvmManager，多 VM 业务编排
├── config.rs                       # 客户机 TOML 配置解析、默认 VM 初始化
├── images/                         # 客户机镜像读取、Linux/x86 boot image 处理
├── fdt/                            # 非 x86 架构客户机 FDT 解析/生成
└── shell/                          # 管理 shell 和命令分发
```

`AxvmManager` 的职责：

```text
初始化 AxVM runtime
加载默认 VM 配置
创建并注册 VM
启动默认 VM 集合
响应 shell 的 start / stop / resume / remove / list 命令
处理普通文件读取、镜像大小查询、配置目录扫描
处理 x86 guest passthrough 前的 host filesystem release 策略
通过受控 helper 为 FDT 生成获取 host boot FDT 信息
```

`os/axvisor` 允许使用 std-like API：

```rust
ax_std::{fs, io, println, thread, time}
```

`os/axvisor` 禁止直接使用底层 host 能力入口：

```rust
ax_api
ax_hal
ax_task
ax_alloc
ax_std::os::arceos
std::os::arceos
```

配置和镜像边界：

```text
属于 os/axvisor:
    配置文件扫描
    默认路径策略
    TOML 字符串读取
    客户机镜像读取
    Linux boot params / multiboot / FDT 生成
    根据配置构造 AxVM / VMMemoryRegion / vCPU 参数

不属于 axvm:
    配置文件在哪里
    镜像从文件系统还是内置二进制读取
    默认启动哪些 VM
    shell 命令如何命名
```

### 2.3 `axvm`：单 VM 抽象和 runtime 原语层

`axvm` 不是最上层多 VM 产品管理器。它提供 VM 抽象、VM runtime 原语、组件接线和私有 host 能力适配。

当前目录职责：

```text
virtualization/axvm/src/
├── lib.rs                         # 对外导出 VM 类型、runtime 原语、注册/查询入口
├── manager.rs                     # AxvmRuntime，VM registry，当前 vCPU/VM 查询
├── vm.rs                          # AxVM，单 VM 状态、memory region、boot/shutdown
├── vcpu.rs                        # AxVM vCPU runtime wrapper 和架构 vCPU 选择
├── task.rs                        # vCPU task extension
├── percpu.rs                      # per-cpu virtualization state 初始化
├── timer.rs                       # AxVM timer event glue
├── cache.rs                       # cache maintenance wrapper
├── arch.rs                        # 架构相关 runtime glue 和组件 HostIf 实现
├── config.rs                      # AxVMConfig 到 AxVM 构造所需的基础转换类型
├── host/
│   ├── mod.rs                     # 私有 host boundary
│   ├── traits.rs                  # HostMemory / HostTime / HostCpu / HostPlatform / ...
│   ├── arceos.rs                  # ArceOS kernel API adapter
│   ├── gic.rs                     # AArch64 host GIC driver access
│   └── paging.rs                  # 基于 HostMemory 的 guest page table handler
└── runtime/
    ├── mod.rs                     # VM runtime loop 和 start/stop/resume/remove 原语
    ├── vcpus.rs                   # vCPU task 创建、唤醒、VM exit 主循环
    ├── hvc.rs                     # hypercall 分发
    ├── ivc.rs                     # inter-VM communication runtime glue
    └── x86_irq.rs                 # x86 IOAPIC/PIT/serial IRQ forwarding glue
```

`AxvmRuntime` 的职责：

```text
初始化默认 ArceOS-backed runtime
检查硬件虚拟化支持
在所有 CPU 上启用硬件虚拟化
初始化已注册 VM 的 vCPU task
启动已注册 VM
提供 start / stop / resume / remove / with_vm 等运行时原语
维护 VM registry 作为 runtime 内部状态
```

`VM_REGISTRY` 是 runtime 运行状态，不是产品层编排策略。它只保存已经由 `os/axvisor` 创建并注册进来的 VM，不负责读取配置、加载镜像或决定默认启动集合。

`axvm` 对外保留的核心入口：

```rust
pub struct AxvmRuntime { ... }

impl AxvmRuntime {
    pub fn new() -> AxResult<Self>;
    pub fn init_vms(&self);
    pub fn start_default_vms(&self);
    pub fn with_vm<T>(vm_id: VMId, f: impl FnOnce(AxVMRef) -> T) -> Option<T>;
    pub fn start_vm(vm_id: VMId) -> AxResult;
    pub fn stop_vm(vm_id: VMId) -> AxResult;
    pub fn resume_vm(vm_id: VMId) -> AxResult;
    pub fn remove_vm(vm_id: VMId) -> Option<AxVMRef>;
}

pub fn register_vm(vm: AxVMRef) -> bool;
pub fn setup_primary_vcpu(vm: AxVMRef);
pub fn get_vm_by_id(vm_id: VMId) -> Option<AxVMRef>;
pub fn get_vm_list() -> Vec<AxVMRef>;
pub fn check_timer_events();
pub fn clean_dcache_range(addr: VirtAddr, size: usize);
```

`axvm` 对外保留的受控 host helper：

```rust
pub use host::paging::HostPagingHandler;

pub fn host_fdt_bootarg() -> usize;
pub fn host_phys_to_virt(paddr: PhysAddr) -> VirtAddr;
pub fn shutdown_host_filesystems() -> AxResult;
```

命名约束：

```text
axvm 不再对外提供 ArceOsHost
axvm public API 不暴露 host::arceos
axvm 不提供 os::arceos feature 或 public adapter
axvm 不使用 runtime/devices 这种泛化目录承载 x86 IRQ glue
```

### 2.4 `axvm::host`：私有 Host boundary

`axvm/src/host` 是 `axvm` 内部访问 host 能力的唯一边界。`host/arceos.rs` 负责 ArceOS kernel API，`host/gic.rs` 负责 AArch64 GIC driver；其他 `axvm` 模块应通过 `host::default_host()`、`Host*` trait 或更窄的 host helper 访问这些能力。

```text
host/traits.rs
    定义 HostMemory / HostTime / HostCpu / HostConsole / HostPlatform

host/mod.rs
    维护私有 host boundary
    暴露 crate-private default_host() facade

host/arceos.rs
    实现默认 ArceOS host adapter
    允许使用 ax_std::os::arceos::{api, modules}

host/task.rs
    包装必要的 ArceOS task / wait queue / CPU mask 类型
    避免 task.rs / runtime/* 直接依赖 host::arceos

host/gic.rs
    AArch64 下集中访问 host GIC driver
    读取 GICD/GICR base、GICD IIDR/TYPER
    处理 host IRQ dispatch 委托和虚拟中断注入

host/paging.rs
    基于 HostMemory 实现 guest page table handler
```

当前 Host trait 按实际需要拆分：

```text
HostMemory
    frame、连续页、phys/virt 转换

HostTime
    monotonic time、timer register/cancel、oneshot timer、tick/nanos 转换

HostCpu
    CPU 数量、当前 CPU、CPU affinity

HostConsole
    x86_64 下的 raw console bytes read/write

HostPlatform
    硬件虚拟化检测、当前 CPU / 全 CPU 虚拟化启用
```

不再保留没有实际抽象收益的 `HostTask` 聚合 trait。vCPU task 当前仍是 ArceOS-backed runtime 细节，由 `host/task.rs` 通过 crate-private wrapper 提供给 `axvm::task` 和 `axvm::runtime::vcpus`。

边界规则：

```text
允许:
    virtualization/axvm/src/host/arceos.rs
        -> ax_std::os::arceos::{api, modules}
    virtualization/axvm/src/host/gic.rs
        -> rdrive / rdif_intc / arm_gic_driver

禁止:
    virtualization/axvm/src/vm.rs
    virtualization/axvm/src/vcpu.rs
    virtualization/axvm/src/runtime/*
    virtualization/axvm/src/timer.rs
    virtualization/axvm/src/percpu.rs
    virtualization/axvm/src/arch.rs
        -> 直接使用 ax_std::os::arceos / ax_hal / ax_task / ax_alloc / rdrive / rdif_intc / arm_gic_driver
```

`arch.rs` 只负责实现各组件的 `*HostIf` glue。通用 memory/time/cpu 能力通过 `host::default_host()` 和 `Host*` trait 获取；需要访问 ArceOS 专用 API 或板级 driver 时，应继续下沉到 `host/arceos.rs`、`host/gic.rs` 或后续更窄的 host adapter 模块。

### 2.5 `axvm::runtime`

`runtime` 承担 VM 执行时原语，不承担设备模型定义或产品配置策略。

```text
runtime/mod.rs
    初始化已注册 VM 的 vCPU runtime state
    启动已注册 VM
    等待默认 VM 集合退出
    提供 start_vm / stop_vm / resume_vm / remove_vm

runtime/vcpus.rs
    创建 vCPU task
    管理每个 VM 的 vCPU wait queue
    处理 vCPU 主循环
    分发 VM exit

runtime/hvc.rs
    处理 hypercall

runtime/ivc.rs
    处理 inter-VM communication runtime glue

runtime/x86_irq.rs
    只处理 x86_64 的 IOAPIC/PIT/serial IRQ forwarding
    不是通用 device 目录
```

`runtime/x86_irq.rs` 的存在意义是把 x86 平台的 passthrough IRQ 转发、PIT IRQ0 注入、serial IRQ pending 检查、IOAPIC EOI 后补注入等逻辑从 vCPU 主循环中拆出去。它不应被命名为 `devices`，因为真正的设备模型仍属于 `axdevice`、`x86_vlapic`、`arm_vgic`、`riscv_vplic` 等组件。

### 2.6 `axvm-types`

`axvm-types` 是轻量 `no_std` 基础 crate，只放共享基础类型，不放 capability API：

```text
VMId
VCpuId
InterruptVector
MAX_VCPU_NUM
VCpuSet
HostPhysAddr
HostVirtAddr
GuestPhysAddr
GuestVirtAddr
GuestPhysAddrRange
GuestVirtAddrRange
AxVmResult / AxVmError alias
```

用途：

- 替代旧的 `axvisor_api::vmm::{VMId, VCpuId, InterruptVector}`。
- 避免 `axvcpu`、irqchip、device 为了 ID 类型依赖 `axvm`。
- 保持组件之间的类型一致。

### 2.7 `axvcpu`

`axvcpu` 只保留通用 vCPU 抽象：

```text
VCpuState
AxVCpuExitReason
AxVCpu trait
ArchVCpu trait
PerCpu trait
register state 抽象
```

依赖：

```text
axvm-types
core / alloc
必要的架构无关基础 crate
```

禁止依赖：

```text
axvm
ax_std
ax_api
ax_hal
ax_task
axvisor_api
```

### 2.8 架构 vCPU 组件

包括：

```text
arm_vcpu
x86_vcpu
riscv_vcpu
loongarch_vcpu
```

职责：

```text
hardware capability check primitive
hardware enable primitive
VM entry/exit
register state
interrupt injection primitive
architecture-specific exit decode
```

这些组件不访问 host memory/time/task/irq。需要外部能力时，定义组件本地 `HostIf`，由 `axvm/src/arch.rs` 通过 `ax_crate_interface` 注入。

当前接口形态：

```text
x86_vcpu::host::X86VcpuHostIf
riscv_vcpu::host::RiscvVcpuHostIf
loongarch_vcpu::host::LoongArchVcpuHostIf
arm_vcpu::host::ArmVcpuHostIf
```

### 2.9 irqchip / timer / device 组件

包括：

```text
arm_vgic
x86_vlapic
riscv_vplic
axdevice_base
axdevice
```

职责：

```text
virtual irqchip model
virtual timer model
MMIO/PIO dispatch
device model composition
```

这些组件不访问 host memory/time/vmm/console。需要能力时定义本地 `HostIf`，由 `axvm` 在 arch glue 或 runtime glue 中注入。

当前接口形态：

```text
x86_vlapic::host::X86VlapicHostIf
riscv_vplic::host::RiscvVplicHostIf
arm_vgic::host::ArmVgicHostIf
```

`x86_vlapic` 的 APIC timer 已采用 generation 和共享原子状态处理旧 timer callback 误触发问题：

```text
ApicTimer
    -> 每次 start / stop / restart 递增 generation
    -> callback 触发时校验 generation
    -> LVT / interval / deadline 放入 shared atomic state
    -> periodic timer 根据当前时间推进下一次 deadline
```

这样可以避免 timer 已经被停止或重启后，旧 callback 仍向 guest 注入过期中断。

### 2.10 `axaddrspace`

`axaddrspace` 继续作为 guest address-space 组件，并移动到 `virtualization/axaddrspace`。

它不直接访问 OS API。需要 host frame/page table 能力时，由 `axvm` 提供：

```text
HostPagingHandler
    -> axvm::host::paging
    -> axvm::host::HostMemory
    -> axvm::host::default_host()
    -> private ArceOS host adapter
```

### 2.11 Cargo feature 传递关系

不引入 `os-arceos` feature。当前默认产品就是 ArceOS-backed Axvisor，host adapter 是 `axvm` 私有实现。

`os/axvisor` feature 只转发顶层产品能力。当前实际 feature 传递如下：

```toml
[features]
default = []
ept-level-4 = ["axvm/4-level-ept"]
fs = ["ax-std/fs", "axvm/host-fs"]
vmx = ["axvm/vmx"]
svm = ["axvm/svm"]
sstc = ["axvm/sstc"]
dyn-plat = ["axvm/plat-dyn", "ax-std/plat-dyn", "ax-driver/plat-dyn"]

x86-qemu-q35 = ["ax-std/x86-qemu-q35"]
aarch64-qemu-virt = ["ax-std/aarch64-qemu-virt"]
riscv64-qemu-virt = ["ax-std/riscv64-qemu-virt"]
loongarch64-qemu-virt = ["ax-std/loongarch64-qemu-virt"]

rockchip-soc = ["ax-driver/rockchip-soc"]
rockchip-sdhci = ["ax-driver/rockchip-sdhci", "ax-driver/rockchip-soc"]
rockchip-dwmmc = ["ax-driver/rockchip-dwmmc", "ax-driver/rockchip-soc"]
phytium-mci = ["ax-driver/phytium-mci"]
sdmmc = [
    "ax-driver/rockchip-sdhci",
    "ax-driver/rockchip-soc",
    "ax-driver/phytium-mci",
]
rockchip-pm = ["ax-driver/rockchip-pm"]
serial = []
```

也就是说，`vmx`、`svm`、`sstc`、`ept-level-4` 等虚拟化能力转发到 `axvm`；静态平台 feature 当前由 Axvisor 顶层转发到 `ax-std/<platform>`，而不是转发到 `axvm/<platform>`。

板级 driver feature 由 Axvisor 顶层转发到 `ax-driver`。例如 OrangePi 5 Plus board build 当前使用 `dyn-plat,fs,rockchip-soc,sdmmc`，其中 `rockchip-soc` / `sdmmc` 不属于 `axvm` 的 runtime feature。

`axvm` feature 当前保留自身的平台 feature，并继续传递到 `ax-std` 和虚拟化组件。但 Axvisor 顶层静态平台 feature 目前没有转发到这些 `axvm/<platform>` feature，因此这些 `axvm` 平台 feature 更像保留给独立构建或后续统一策略的入口，不能视为 Axvisor 主路径必经链路。

```toml
[features]
default = ["vmx"]
fs = ["vmx", "ax-std/fs"]
host-fs = ["ax-std/fs"]
vmx = ["axaddrspace/vmx", "x86_vcpu/vmx"]
svm = ["axaddrspace/svm", "x86_vcpu/svm"]
sstc = ["vmx", "riscv_vcpu/sstc"]
4-level-ept = ["vmx"]
plat-dyn = ["vmx", "ax-std/plat-dyn", "dep:axplat-dyn"]

x86-qemu-q35 = ["vmx", "ax-std/x86-qemu-q35"]
aarch64-qemu-virt = ["vmx", "ax-std/aarch64-qemu-virt"]
riscv64-qemu-virt = ["vmx", "ax-std/riscv64-qemu-virt"]
loongarch64-qemu-virt = ["vmx", "ax-std/loongarch64-qemu-virt"]
```

`axvm` 固定依赖 ArceOS host 所需基础能力：

```toml
ax-std = { workspace = true, features = [
    "paging",
    "irq",
    "multitask",
    "task-ext",
    "smp",
    "hv",
] }
```

静态平台 feature 传递链：

```text
axvisor/<platform>
    -> ax-std/<platform>
        -> ax-feat/<platform>
            -> ax-hal/<platform>
                -> ax-plat-*
```

动态平台 feature 传递链：

```text
axvisor/dyn-plat
    -> axvm/plat-dyn
    -> ax-std/plat-dyn
    -> ax-driver/plat-dyn
```

基础能力传递链：

```text
axvm
    -> ax-std[paging, irq, multitask, task-ext, smp, hv]

ax-std/paging
    -> ax-feat/paging
    -> ax-std/alloc

ax-std/irq
    -> ax-api/irq
    -> ax-feat/irq

ax-std/multitask
    -> ax-api/multitask
    -> ax-feat/multitask

ax-std/task-ext
    -> ax-feat/task-ext

ax-std/smp
    -> ax-feat/smp
    -> ax-kspin/smp

ax-std/hv
    -> ax-feat/hv
```

### 2.12 axbuild 调整

`cargo xtask axvisor ...` 生成 feature 时只生成 Axvisor 顶层 feature：

```text
板级平台:
    生成 axvisor/<platform>
    不生成 ax-hal/<platform>
    不生成 ax-feat/<platform>

动态平台:
    生成 dyn-plat
    dyn-plat 再传递到 axvm/plat-dyn、ax-std/plat-dyn、ax-driver/plat-dyn

业务能力:
    fs -> axvisor/fs

x86 backend:
    vmx / svm 作为 axvisor feature
    再转发到 axvm/vmx 或 axvm/svm
```

边界：

```text
scripts/axbuild 不把 ax-hal/<platform> 或 ax-feat/<platform> 写入 Axvisor 构建配置
scripts/axbuild 不直接关心 ax-feat / ax-hal 平台 feature 细节
scripts/axbuild 只生成 Axvisor 顶层平台 feature
```

兼容和校验规则：

```text
允许:
    x86-qemu-q35
    aarch64-qemu-virt
    riscv64-qemu-virt
    loongarch64-qemu-virt
    dyn-plat
    ax-std/<platform>    # 作为旧写法被规范化为 Axvisor 顶层 feature

拒绝:
    ax-feat/<platform>
    ax-hal/<platform>
```

### 2.13 验证要求

修改代码后按影响范围分层验证。

最小验证：

```bash
cargo fmt
cargo xtask clippy --package <changed-crate>
```

涉及共享虚拟化组件或跨 crate 接口时，应扩展到相关包：

```bash
cargo xtask clippy --package axvm-types
cargo xtask clippy --package axaddrspace
cargo xtask clippy --package axvcpu
cargo xtask clippy --package axvm
cargo xtask clippy --package arm_vcpu
cargo xtask clippy --package x86_vcpu
cargo xtask clippy --package riscv_vcpu
cargo xtask clippy --package loongarch_vcpu
cargo xtask clippy --package arm_vgic
cargo xtask clippy --package x86_vlapic
cargo xtask clippy --package riscv_vplic
cargo xtask clippy --package axdevice
cargo xtask clippy --package axbuild
```

`axvisor` 需要通过 Axvisor xtask build config 验证，普通 `cargo xtask clippy --package axvisor` 会因缺少目标/构建配置而跳过。

涉及 AArch64 GIC、VGIC、vCPU host callback 或 board feature 时，至少验证：

```bash
cargo xtask axvisor build --config test-suit/axvisor/normal/board-orangepi-5-plus/build-aarch64-unknown-none-softfloat.toml
```

真实 board test 路径在硬件可用时继续验证：

```bash
cargo xtask axvisor test board --board orangepi-5-plus-linux
```

发布或合并前建议补充 QEMU 验证：

```bash
cargo xtask axvisor qemu --arch x86_64
cargo xtask axvisor qemu --arch aarch64
cargo xtask axvisor qemu --arch riscv64
cargo xtask axvisor qemu --arch loongarch64
```

至少 x86_64 应启动到 Axvisor shell；其余架构如受工具链、QEMU 环境或板级资源限制，需要记录具体阻塞原因。

## 3. 真实遗留问题

### 3.1 Host trait 是否应该长期留在 axvm

本阶段 `Host` trait 留在 `axvm` 内部，因为这些能力直接服务于 VM runtime，且当前默认产品就是 ArceOS-backed Axvisor。

如果未来需要多个 runtime 复用同一 host capability，再考虑拆分：

```text
virtualization/axvm-host-api
```

但本阶段不提前拆，避免形成新的全局中间层。

### 3.2 ArceOS host adapter 是否要拆 crate

本阶段不拆。`ArceOsHost` 是 `axvm` 私有实现，外部只看到 `AxvmRuntime`、VM 类型和少量受控 helper。

未来若需要非 ArceOS host，再考虑：

```text
axvm-core
axvm-arceos
axvm-linux
```

### 3.3 受控 host helper 的边界是否继续收紧

当前 `axvm` 对外保留：

```rust
HostPagingHandler
host_fdt_bootarg()
host_phys_to_virt()
shutdown_host_filesystems()
```

这些 helper 是为了支持 FDT 生成、guest paging handler 和 x86 guest passthrough 前释放 host filesystem。它们不等价于公开 host adapter，但仍是需要长期审视的边界。

后续如果需要进一步收紧，可以把 FDT host 信息读取和 filesystem release 包装为更明确的 Axvisor-facing service API。

### 3.4 AArch64 GIC host adapter 的板级验证

AArch64 的 vCPU / VGIC host callback 由 `axvm/src/arch.rs` 接入，并把真正的 host GIC driver 访问集中到 `axvm/src/host/gic.rs`，包括：

```text
virtual interrupt injection
IRQ fetch
current EL IRQ dispatch
current vCPU id
host GICD / GICR base discovery
VGICD IIDR / TYPER 读取
```

这些能力已经不应再停留在 `todo!()`。当前 EL IRQ 处理由 `host/gic.rs` 委托给 ArceOS 平台 IRQ handler；现有 AArch64 GIC-backed 平台会在平台 handler 内部 ack/dispatch/EOI，因此 `axvm` 不应先手动 ack 再调用该 handler。后续重点是继续用 QEMU 和真实板级 Linux guest 验证 GICv2/GICv3 行为，尤其是 passthrough interrupt、virtual timer interrupt 和 GICR per-CPU base 计算。

当前 VM-exit 外部中断路径同样不能先由 `axvm` 手动 GIC ack 再交给 ArceOS 平台 handler。AArch64 下 `ArmVcpuHostIf::fetch_irq()` 应保持“处理当前 IRQ”的语义，`axvm::runtime::vcpus` 不再对 AArch64 的 `ExternalInterrupt` 进行第二次 `dispatch_host_irq()`。后续若需要把真实 IRQ vector 返回给 runtime，应同时提供“已 ack IRQ 的 dispatch + EOI”接口，不能复用会再次 ack 的平台 handler。

需要继续注意的是，AArch64 host callback 中的 `todo!()` 已经移除，但 `arm_vgic` 的部分 MMIO/sysreg 行为仍未完整实现，例如 VGICR/GICD 未覆盖寄存器和 virtual timer read 路径。真实 Linux guest 仍可能因为访问这些寄存器而触发 panic 或语义不一致。

### 3.5 axvisor_api 源码删除后的文档残留

主线源码和 Cargo 依赖中已经不再使用 `axvisor_api` / `axvisor_api_proc`，对应源码目录也已经不存在。但 `docs/docs/components/*` 等生成或组件说明文档仍可能保留旧的 `axvisor_api` 依赖图和 crate 页面。

后续应重新生成或清理这些组件文档，否则仓库级文档会继续显示已经删除的 API 层。

### 3.6 axvm 平台 feature 是否长期保留

`axvm` 当前仍声明：

```toml
x86-qemu-q35 = ["vmx", "ax-std/x86-qemu-q35"]
aarch64-qemu-virt = ["vmx", "ax-std/aarch64-qemu-virt"]
riscv64-qemu-virt = ["vmx", "ax-std/riscv64-qemu-virt"]
loongarch64-qemu-virt = ["vmx", "ax-std/loongarch64-qemu-virt"]
```

但 Axvisor 顶层静态平台 feature 当前没有转发到这些 `axvm/<platform>` feature。后续建议二选一，避免双路径漂移：

```text
若没有 axvm 独立构建场景:
    删除 axvm 静态平台 feature

若需要保留 axvm 独立构建场景:
    让 axvisor/<platform> 也转发到 axvm/<platform>
    并明确 axvm/<platform> 是 axvm 独立使用入口
```

### 3.7 Rust native std 支持

本方案不直接实现 `target_os = "arceos"` native std。

短期：

```text
os/axvisor 业务层
    -> ax_std::{fs, io, thread, ...}

axvm/src/host/arceos.rs
    -> ax_std::os::arceos::{api, modules}
```

长期：

```text
os/axvisor 业务层
    -> std::{fs, io, thread, ...}

axvm/src/host/arceos.rs
    -> std::os::arceos 或更底层 ArceOS host API
```
