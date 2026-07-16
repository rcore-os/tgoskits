---
sidebar_position: 3.5
sidebar_label: "somehal 运行时 HAL"
---

# somehal 运行时 HAL

`platforms/somehal` 是位于 `someboot` 与 `axplat-dyn` 之间的多架构运行时 HAL。其设计目标是提供统一的 `PlatOp` 契约，覆盖 AArch64 / LoongArch64 / RISC-V / x86_64，并支持 GICv2/v3 运行时识别、rdrive 设备发现和 ACPI/FDT 双路径。

## crate 概览

`#![no_std]` + `#![cfg_attr(not(test), no_main)]` + `#![feature(used_with_arg)]`。`no_main` 是因为 `somehal` 本身不导出 Rust 入口，而是通过 `someboot::entry` 宏注入。

`platforms/somehal/src/lib.rs` 的模块组织：

```rust
mod boot_console;
pub(crate) mod common;
pub mod cpu;
mod driver;
pub mod irq;
mod irq_routing;
pub mod platform;
pub mod rtc;
pub mod setup;

#[cfg(target_arch = "...")]
pub mod arch; // 按目标架构条件编译

pub use boot_console::{ConsoleDeviceIdError, device_id as console_device_id};
pub use page_table_generic::{PagingError, PagingResult};
pub use platform::platform_name;
pub use setup::KernelOp;
pub use someboot::{
    bootargs, console, entry, fdt_addr, fdt_addr_phys, mem, power,
    rsdp_addr_phys, smp, timer,
};
pub use somehal_macros::somehal_secondary_entry as secondary_entry;

pub fn init(kernel: &'static dyn KernelOp) {
    setup::set_kernel_op(kernel);
}

pub fn post_paging() {
    someboot::post_allocator();
    driver::rdrive_setup();
}
```

默认 secondary entry 是个 spin loop：

```rust
#[unsafe(no_mangle)]
pub fn __somehal_secondary_default() -> ! { loop { core::hint::spin_loop(); } }
```

真正的 secondary 入口在 `someboot::secondary_entry` 装饰的函数里完成页表切换、`arch::Plat::secondary_init{,_intc,_systick}`，再跳到内核侧 `__somehal_secondary`。

## 公共 re-export 与入口函数

| 来源 | 导出 |
| --- | --- |
| `someboot` | `bootargs`、`console`、`entry`、`fdt_addr(_phys)`、`mem`、`power`、`rsdp_addr_phys`、`smp`、`timer` |
| `somehal-macros` | `somehal_secondary_entry` → 别名 `secondary_entry` |
| `page-table-generic` | `PagingError`、`PagingResult` |
| `setup` | `KernelOp` trait，以及 `MmioOp`/`MmioAddr`/`MmioRaw`/`MapError`（re-export 自 `mmio-api`） |
| `platform` | `platform_name() -> Option<&'static str>` |
| `boot_console` | `ConsoleDeviceIdError`、`device_id()` |
| `cpu` | `current_cpu_idx() -> Option<usize>` |
| `rtc` | `epoch_time_nanos() -> Option<u64>` |

## setup.rs — `KernelOp` 契约

`platforms/somehal/src/setup.rs`：

```rust
pub use mmio_api::{MapError, MmioAddr, MmioOp, MmioRaw};

pub trait KernelOp: MmioOp {
    fn current_cpu_idx(&self) -> Option<usize> { None }
}
```

`set_kernel_op(op: &'static dyn KernelOp)` 把实例写入全局，**同时调用 `mmio_api::init(op)`**，从此所有 driver 内的 `mmio_api::ioremap` 都会回流到这个 kernel 实例的 `MmioOp` 实现。`axplat-dyn` 的 `Kernel` 把它委托给 `axklib::mmio::op()`。

## driver.rs — 设备发现入口

`rdrive_setup()` 是 somehal 的**唯一设备发现入口**，根据 `someboot` 暴露的事实选择 FDT 或 ACPI：

```rust
// platforms/somehal/src/driver.rs（简化）
pub fn rdrive_setup() {
    if let Some(addr) = someboot::fdt_addr() {
        rdrive::init(rdrive::Platform::Fdt {
            addr: NonNull::new(addr).unwrap(),
        }).unwrap();
    } else if let Some(rsdp) = someboot::rsdp_addr_phys() {
        rdrive::init(rdrive::Platform::Acpi(rdrive::probe::acpi::AcpiRoot::new(
            rsdp, someboot::mem::phys_to_virt,
        )));
    } else {
        warn!("No FDT or ACPI RSDP found; skip rdrive initialization");
    }
}
```

随后各架构模块用 `module_driver!`（来自 `rdrive`）注册 probe-time driver：

- ARMv8 通用 timer：`compatible = "arm,armv8-timer"`，见 `platforms/somehal/src/arch/aarch64/systick.rs`。
- x86 ACPI IOAPIC：`AcpiId { hid: "ACPIIOAP", ... }`，见 `platforms/somehal/src/arch/x86_64/mod.rs`。
- AArch64 GIC：通过 `arm_gic_driver` crate 注册，见下文。

## irq.rs — IRQ domain 注册表

`platforms/somehal/src/irq.rs` 维护运行时 IRQ domain 表：

```rust
static IRQ_DOMAINS: Mutex<Vec<IrqDomain>> = Mutex::new(Vec::new());
static X86_IOAPIC_DOMAIN_SLOT: AtomicU16 = AtomicU16::new(INVALID_IRQ_DOMAIN);
// ...每种 kind 一个 atomic 槽

pub enum IrqDomainKind {
    X86IoApic, AArch64Gic, RiscvPlic, LoongArchEioIntc, LoongArchPchPic,
}

pub struct IrqDomain {
    pub id:    IrqDomainId,
    pub owner: DeviceId,
    pub kind:  IrqDomainKind,
}
```

### 注册 API

| 函数 | 作用 |
| --- | --- |
| `alloc_irq_domain(owner, kind)` | 在 `7..u16::MAX` 区间分配新 id |
| `register_irq_domain(owner, preferred, kind)` | 显式指定 id，校验 reserved 段不冲突 |
| `domain_by_id` / `domain_by_owner` / `domain_by_kind` | O(n) 查询 |
| `domain_by_kind_fast` | 使用 per-kind atomic slot，O(1) |
| `intc_by_domain(domain)` | 解析持有该 domain 的 `rdrive` `Device<Intc>` |
| `set_controller_irq_enabled` | 通过 driver 接口开关 IRQ |

### `PlatOp` 契约

`platforms/somehal/src/common.rs` 是每个架构后端必须实现的契约：

```rust
pub trait PlatOp {
    type ActiveIrq;

    fn irq_set_enable(irq: IrqId, enable: bool) -> Result<(), IrqError>;
    fn irq_set_affinity(_irq: IrqId, _aff: IrqAffinity) -> Result<(), IrqError> { Err(Unsupported) }
    fn send_ipi(_irq: IrqId, _target: IpiTarget) { panic!(...) }
    fn ipi_irq() -> IrqId;
    fn begin_irq(raw: usize) -> Option<Self::ActiveIrq>;
    fn active_irq_id(active: &Self::ActiveIrq) -> IrqId;
    fn systick_irq() -> IrqId;
    fn resolve_irq_source(source: IrqSource) -> Result<IrqId, IrqError>;

    fn secondary_init();
    fn secondary_init_intc(cpu_idx: usize);
    fn secondary_init_systick();

    fn send_ipi_to_cpu(cpu_id: usize) { ... }
}
```

外层包装 `ActiveIrq`（`irq.rs` 约 L228）持有架构特定的 `Plat::ActiveIrq`，`Drop` 时调用控制器的 complete/EOI。

### 公共 IRQ free functions

全部转发到 `Plat::*`：`irq_set_enable`、`irq_set_affinity`、`send_ipi`、`ipi_irq`、`systick_irq`、`begin_irq`、`resolve_irq_source`、`send_ipi_to_cpu`。架构扩展：`aarch64_gic_irq_id(_checked)`、`irq_setup_by_fdt`。

## irq_routing.rs — 架构无关 IRQ 工具

`platforms/somehal/src/irq_routing.rs` 是与运行 arch 解耦的辅助逻辑（用 `cfg(any(test, target_arch = ...))` 控制），便于单元测试：

- **LoongArch**：`classify_cpu_irq`、`cpu_local_hwirq_is_runtime_irq`、`AcpiControllerRoutes`（记录 `AcpiGsiRoute` ↔ `IrqId`，供 `controller_input` 反向查找）。
- **RISC-V**：`RISCV_S_*_CAUSE` 常量、`classify_riscv_trap`、`riscv_cpu_local_hwirq_is_runtime_irq`、`riscv_cpu_local_irq_from_raw`、`riscv_local_irq_raw`、`riscv_plic_hwirq_from_source`、`riscv_resolve_controller_line`。

## boot_console.rs — 控制台解析

`platforms/somehal/src/boot_console.rs` 的 `device_id()` 按以下顺序解析硬件 console：

1. **bootargs**：解析 `console=ttyS<n>`、`console=ttyAMA<n>`、`console=tty<n>`、`console=ttynull`。最后一个**硬件** serial 胜出；纯 tty 配置返回 `NoHardwareDevice`。
2. **ACPI SPCR**：通过 `rdrive::acpi_spcr_console_device_id()`，仅 serial index 0。
3. **FDT stdout-path**：读 `/chosen/stdout-path` 或 `linux,stdout-path`，并解析 alias。

该模块有 8 个单元测试覆盖各种组合，是 somehal 中测试最完善的子模块。

## 架构后端

每个后端在 `platforms/somehal/src/arch/<arch>/mod.rs`（见 `platforms/somehal/src/arch`）定义 `pub struct Plat;` 并 `impl PlatOp for Plat`。

### AArch64 (`arch/aarch64/mod.rs`)

- 子模块：`gic`（含 `v2` / `v3`）、`systick`。
- **GIC 版本运行时识别**：`GIC_BACKEND: AtomicU8` 取值 `Backend::{None, V2, V3}`。首次 `init_cpu` 时通过 ICC 支持位自动检测。`gic::init_current_cpu` / `init_cpu(cpu_idx)` 分别处理 BSP 与 secondary。`module_driver!` 通过 `arm_gic_driver` crate 注册 GIC probe。
- **systick**：注册 `arm,armv8-timer` FDT 驱动，捕获 IRQ 向量；从核上运行 `setup_systick_irq()`。
- **`resolve_irq_source`**：`AcpiGsi(gsi)` 和 `AcpiGsiRoute(route)` 直接映射到 GIC hwirq（ARM 上没有 IOAPIC 间接层）。

### RISC-V (`arch/riscv64/mod.rs`)

- 子模块：`plic`。
- CPU-local IRQ（timer/IPI/external）通过 `irq_routing::riscv_*` 分类。
- `ipi_irq() = IrqId::new(CPU_LOCAL_DOMAIN, HwIrq(S_SOFT_CAUSE))`。
- `resolve_percpu` 校验是否为运行时 IRQ cause。
- `send_ipi` 手动遍历 `AllExceptCurrent`。

### LoongArch64 (`arch/loongarch64/mod.rs`)

- 子模块：`eiointc`、`pch_pic`、`irq_common`。
- **IOCSR IPI**：`IOCSR_IPI_SEND = 0x1040`，写入值 `(cpu_id << 16) | vector`，可选阻塞位 `1 << 31`。
- 中断号：`EIOINTC_IRQ = 3`、`IPI_IRQ = 12`。
- `begin_irq` 流程：先 ACK timer/IPI，外部 IRQ 走 `eiointc::claim_irq`，再通过预先发布的 CPU fast-path route 解析 PCH-PIC input；edge child 在返回 `ActiveIrq`、进入 action 前写 PCH `CLEAR` 并执行设备写 `dbar`，level child 不写 `CLEAR`。hard IRQ 不查询 `rdrive`，也不获取 controller 控制面锁。
- `ActiveIrq` 用 `Completion::{None, EioIntc, LioIntc}` 携带 action 后的 parent 完成令牌；PCH edge child 已在 action 前完成 ack，`Drop` 只完成 EIOINTC parent。这样 action 执行期间到达的新 edge 能重新锁存，而 level 的撤销仍由设备负责。
- PCH-PIC probe 在所有 input 仍 masked 时，把 CPU route 与只读 MMIO completion endpoint 合并成一个 write-before-release 的冻结对象，并校验 MMIO 长度、`1..=64` input 数量及 8 位向量窗口。该对象持续到关机；当前 fast path 只支持一个 PCH，第二实例在 reset/注册前明确失败。
- ACPI GSI 路由：先 `rdrive::probe::acpi::with_acpi(|s| s.routing().resolve_gsi(gsi))`，再按 `AcpiGsiController::PchPic` 分发。

### x86_64 (`arch/x86_64/mod.rs`)

- 子模块：`lapic`、`vector`。
- 通过 `module_driver!` 注册 ACPI IOAPIC 驱动，`AcpiId { hid: "ACPIIOAP", ... }`。
- `struct X86IoApicIntc` 实现 `rdif_intc::Interface::{translate_acpi, supports_acpi_gsi, configure_acpi}`。
- `AcpiGsiRoute` 只携带固件 GSI、controller identity、controller-local input、trigger 和 polarity，不携带 CPU vector；rdrive 不从 GSI 推导 vector。
- control plane 为每条有效路由独立分配 external vector，并保存预分配的 `ProgrammedIoApicRoute`；低 GSI 的 `0x30 + gsi` 仅作为 IOAPIC 内部可选偏好，冲突或越界时扫描合法空闲 vector。
- CPU/IRQ fast path 使用固定容量 endpoint slot，以完整 `u32` GSI 为 key，并用独立的 vector reverse map 从 trap vector 恢复 `IrqId`。查找有固定上界，不访问 rdrive，也不分配。
- 配置顺序固定为“验证并预留 endpoint/vector → 在全局 IRQ-safe MMIO lock 下写 masked redirection entry → 依次 Release 发布 vector 和 endpoint”；预留 token 在提交前失败时自动回滚。
- 每个 IOAPIC 的 redirection 表初始化为全部 MASKED；`MASKED_IOAPIC_PLACEHOLDER_VECTOR = 0x21`。
- IOREGSEL/IOWIN 的 probe、配置、affinity 和 hard-IRQ mask/unmask 访问共享同一把 `SpinIrqSave`，避免多个 IOAPIC 对象或 CPU 交错破坏间接 MMIO transaction。
- x86 early console 向上报告 COM1 GSI 4；只有 IOAPIC control plane 可以把它分配为 CPU vector。

## build.rs

`platforms/somehal/build.rs` 把 `link.ld` 拷贝到 `${OUT_DIR}/link.x`。`link.ld` 内 `INCLUDE "someboot.x"` 并提供 `__someboot_secondary` / `__somehal_secondary` 的默认回退实现。

## 与上下游的契约

- **上游（`someboot`）**：依赖 `someboot` 暴露的 `fdt_addr` / `rsdp_addr_phys` / `mem::phys_to_virt` / `smp::cpu_meta_list` / `rtc::epoch_time_nanos` 等。
- **下游（`axplat-dyn`）**：通过 `somehal::init`、`somehal::post_paging`、`somehal::irq::begin_irq` 等被消费；`KernelOp` 是 `axplat-dyn` 必须实现的 trait。
- **架构无关 helper**：`PlatOp`、`IrqDomain`、`boot_console`、`irq_routing` 都被多个 arch 共享，新增架构只需实现 `PlatOp` 并在 `arch/<new>/mod.rs` 中注册 probe driver。

## 扩展指引

- **新增架构**：创建 `src/arch/<arch>/`，定义 `pub struct Plat;` 并实现 `PlatOp`；按需在 `irq_routing.rs` 添加架构无关 helper；通过 `module_driver!` 注册中断控制器。
- **新增 IRQ domain**：在 `IrqDomainKind` 加枚举值，给 arch 后端在合适时机调用 `register_irq_domain` / `alloc_irq_domain`；添加 per-kind atomic slot 以走 fast path。
- **新增 console 解析路径**：在 `boot_console.rs` 的 `device_id()` 中按优先级追加；务必补单元测试。
