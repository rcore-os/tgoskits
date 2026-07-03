---
sidebar_position: 4
sidebar_label: "设备发现"
---

# 设备发现

平台选择和设备发现是两件事。平台 crate 负责实现 `ax-plat`；设备发现则通过 `rdrive` 和各类 probe 来源把硬件注册成上层可消费的设备能力。

## 入口：`rdrive_setup`

`somehal` 是仓库内默认的设备发现驱动器。其唯一入口在 `platforms/somehal/src/driver.rs`：

```rust
pub fn rdrive_setup() {
    if let Some(addr) = someboot::fdt_addr() {
        rdrive::init(rdrive::Platform::Fdt {
            addr: NonNull::new(addr).unwrap(),
        }).unwrap();
    } else if let Some(rsdp) = someboot::rsdp_addr_phys() {
        rdrive::init(rdrive::Platform::Acpi(
            rdrive::probe::acpi::AcpiRoot::new(rsdp, someboot::mem::phys_to_virt)
        ));
    } else {
        warn!("No FDT or ACPI RSDP found; skip rdrive initialization");
    }
}
```

`rdrive_setup()` 由 `axplat-dyn` 的 `init_later` 经 `somehal::post_paging()` 间接调用（见 [dynamic.md](dynamic.md)）。外部平台也可以自行调用 `rdrive::init(rdrive::Platform::Static)`，由平台代码注册静态设备。

随后 `platforms/axplat-dyn/src/drivers/mod.rs` 调用：

```rust
pub fn probe_all_devices() -> Result<(), AxError> {
    if !rdrive::is_initialized() {
        warn!("rdrive is not initialized; skip platform device probe");
        return Ok(());
    }
    rdrive::probe_all(false).map_err(|_| AxError::BadState)
}
```

## Probe 来源

| 来源 | 用途 | 典型平台 |
| --- | --- | --- |
| Static | 板级 glue 显式注册设备，没有固件描述时使用 | 自定义平台、早期 bring-up |
| FDT | 从设备树发现 MMIO、IRQ、compatible 等资源 | RISC-V/AArch64 QEMU、嵌入式板卡 |
| ACPI | 从 ACPI namespace 和资源表发现设备 | x86_64、UEFI 平台 |
| PCI | 枚举 PCI/PCIe 设备、BAR、INTx/MSI 等 | QEMU、PC、部分 SoC root complex |

`rdrive::Platform::Static` 或 `ProbeKind::Static` 只是设备发现来源，不是旧的 `myplat` / `defplat` Cargo feature 平台选择机制。

## 静态注册示例

没有固件描述的平台可以在平台初始化后注册静态 probe：

```rust
rdrive::register_add(rdrive::register::DriverRegister {
    name: "custom-uart",
    level: rdrive::register::ProbeLevel::PostKernel,
    priority: rdrive::register::ProbePriority::DEFAULT,
    probe_kinds: &[rdrive::register::ProbeKind::Static {
        on_probe: probe_uart,
    }],
});
```

随后平台 later init 可调用：

```rust
let _ = rdrive::init(rdrive::Platform::Static);
```

## 内置驱动声明

somehal 的架构后端通过 `rdrive::module_driver!` 注册一系列内置 driver，这些 driver 在 `probe_all` 时被自动匹配：

| 驱动 | compatible / AcpiId | 源码 |
| --- | --- | --- |
| ARMv8 通用 timer | `arm,armv8-timer` | `platforms/somehal/src/arch/aarch64/systick.rs` |
| ARM GIC（v2/v3） | 由 `arm_gic_driver` crate 注册 | `platforms/somehal/src/arch/aarch64/gic/mod.rs` |
| x86 ACPI IOAPIC | `ACPIIOAP` | `platforms/somehal/src/arch/x86_64/mod.rs` |

`module_driver!` 在编译期生成一个 `DriverRegister` 项并放到 `.init_array`/特殊段中，`rdrive::init` 完成后会扫描这些项并建立索引。

## IRQ 解析

设备绑定信息可以携带已经解析好的 `IrqId`，也可以携带待平台解析的 firmware source。真正注册 handler 前应由平台 resolver 完成转换：

- FDT interrupt specifier 保留 controller owner 和原始 cells。
- ACPI PCI INTx route 保留 trigger、polarity、controller 和 input。
- PCI fallback 的 legacy interrupt line 只作为兼容路径。
- 运行时注册 handler 时使用 `ax_hal::irq::resolve_irq_source(...)` 或平台等价 resolver。

`ax_plat::irq::IrqSource` 是源码侧的判别枚举，区分 legacy、ACPI GSI、PCI INTx、CPU-local、percpu 等。平台 resolver 不应通过向量加减、固定偏移或裸数字猜测 IRQ。缺失、malformed 或不支持的 source 应返回错误，具体 resolver 行为见 [somehal.md](somehal.md) 中各架构 `PlatOp::resolve_irq_source` 的实现。

### IRQ domain 与设备绑定

`platforms/somehal/src/irq.rs` 维护 `IRQ_DOMAINS: Mutex<Vec<IrqDomain>>`，每个 domain 归属于一个 `rdrive::Device<Intc>`。当驱动需要把硬件 IRQ 转成全局 `IrqId` 时：

1. 通过 `domain_by_kind_fast(IrqDomainKind::AArch64Gic)` 等拿到 domain id。
2. `IrqId::new(domain_id, HwIrq(hwirq))` 构造全局 IRQ 标识。
3. `ax_plat::irq::request_irq(irq, handler, ...)` 注册处理函数。

`intc_by_domain(domain)` 反向解析出 driver 实例，用于 `set_controller_irq_enabled` 等控制操作。

## 与 rdrive/rdif 的关系

`rdrive` 负责 probe 调度和设备 registry；`rdif-*` 负责能力边界，例如 block、net、display、input、intc、timer、serial。平台 glue 应把 MMIO、DMA、IRQ 和 firmware metadata 保持在边界处，不把 OS runtime 细节泄漏进 portable driver core。

具体的能力契约：

- **MMIO**：driver core 通过 `mmio_api::ioremap` 申请映射；`somehal::init(kernel)` 已把 `KernelOp` 注册到 `mmio_api`，ioremap 会回流到内核地址空间管理器（`axplat-dyn` 中是 `axklib::mmio::op()`）。
- **DMA**：通过 `dma-api` 跨边界，不与 driver core 耦合。
- **IRQ**：通过 `rdif-intc` 表达 controller 能力；`somehal` 把硬件 IRQ 翻译成 `ax_plat::irq::IrqId` 后交给上层。
- **Firmware metadata**：FDT 节点指针、ACPI 资源描述应停留在 probe boundary，不进入 driver 内部数据结构。

更完整的驱动路径见 [驱动框架](../driver/overview.md)。
