<!-- refreshed: 2026-05-13 -->
# Architecture

**Analysis Date:** 2026-05-13

## System Overview

TGOSKits is a monorepo that consolidates 60+ standalone component repositories into a single Cargo workspace. It contains three operating systems running on a shared platform abstraction and driver model.

```text
+------------------------------------------------------------------+
|                        APPLICATION LAYER                          |
|  ArceOS examples/           StarryOS userland      Axvisor VMs   |
+------------------------------------------------------------------+
|                            OS KERNELS                            |
|  +-------------------+  +------------------+  +-----------------+|
|  | ArceOS (unikernel)|  | StarryOS (mono)  |  |Axvisor (type-1)||
|  | os/arceos/modules/|  |os/StarryOS/kernel|  | os/axvisor/src/ ||
|  +--------+----------+  +--------+---------+  +--------+--------+|
+-----------|---------------------|---------------------|-----------+
|           v                     v                     v           |
|                      SHARED INFRASTRUCTURE                        |
|  +--------------------+  +-------------------+  +---------------+ |
|  | Platform HAL       |  | Driver Traits     |  | Components    | |
|  | components/        |  | components/       |  | components/   | |
|  | axplat_crates/     |  | axdriver_crates/  |  | 50+ crates    | |
|  +--------------------+  +-------------------+  +---------------+ |
|  +--------------------+  +-------------------+                    |
|  | Platform Impls     |  | Driver Impls      |                    |
|  | axplat-* (8 archs)  |  | drivers/ (5 types)|                    |
|  +--------------------+  +-------------------+                    |
+------------------------------------------------------------------+
```

Three systems share platform and driver code but maintain independent kernel logic:

| System | Type | Location | Description |
|--------|------|----------|-------------|
| ArceOS | Unikernel | `os/arceos/` | Modular unikernel with ~15 optional modules |
| StarryOS | Monolithic teaching OS | `os/StarryOS/` | Linux-compatible kernel on ArceOS runtime |
| Axvisor | Type-1 hypervisor | `os/axvisor/` | Lightweight hypervisor for running guest VMs |

## Layering Model

### Layer 1: Platform Abstraction

The platform HAL is defined in `components/axplat_crates/axplat/src/` as a set of traits using `#[def_plat_interface]`. Each trait covers a hardware responsibility:

| Trait Module | File | Responsibility |
|---|---|---|
| `ConsoleIf` | `components/axplat_crates/axplat/src/console.rs` | Console I/O (read/write bytes, IRQ) |
| `InitIf` | `components/axplat_crates/axplat/src/init.rs` | Boot-time initialization (early/late, primary/secondary) |
| `IrqIf` | `components/axplat_crates/axplat/src/irq.rs` | IRQ enable/disable/register/handle |
| `MemIf` | `components/axplat_crates/axplat/src/mem.rs` | Physical memory regions, MMIO, DMA regions |
| `PercpuIf` | `components/axplat_crates/axplat/src/percpu.rs` | Per-CPU data layout |
| `PowerIf` | `components/axplat_crates/axplat/src/power.rs` | Shutdown, reboot |
| `TimeIf` | `components/axplat_crates/axplat/src/time.rs` | Monotonic time, wall time, timer IRQ |

Platform implementations register against these traits via `#[impl_plat_interface]`. There are two dispatch modes:

1. **Static dispatch** (ArceOS, StarryOS): The appropriate platform crate is selected at compile time by depending on exactly one of the 8 arch-specific crates:
   - `axplat-aarch64-qemu-virt`, `axplat-aarch64-raspi`, `axplat-aarch64-bsta1000b`, `axplat-aarch64-phytium-pi` (AArch64)
   - `axplat-riscv64-qemu-virt` (RISC-V)
   - `axplat-x86-pc` (x86_64)
   - `axplat-loongarch64-qemu-virt` (LoongArch)
   - Plus `axplat-aarch64-peripherals` (shared AArch64 peripherals)

2. **Dynamic dispatch** (Axvisor): `platform/axplat-dyn/src/` provides runtime platform selection with feature-gated platform backends. Selected via `dyn-plat` feature.

The ArceOS HAL module `os/arceos/modules/axhal/` wraps the platform traits and re-exports them as a unified API. It also features-forward platform capabilities (smp, irq, fp-simd, etc.).

### Layer 2: Driver Model

Drivers follow a layered separation pattern with three tiers:

| Tier | Location | Purpose |
|------|----------|---------|
| **Driver Core** | `drivers/<type>/<driver>/` | Hardware-specific device logic (register I/O, protocols) |
| **OS Glue** | Via `axdriver_base` traits + `axdriver_*` trait crates | OS-agnostic driver trait implementations |
| **Runtime** | `os/arceos/modules/axdriver/` or kernel-internal | Driver registration, probing, and lifecycle |

Trait definitions reside in `components/axdriver_crates/`:

| Crate | File | Trait | For |
|-------|------|-------|-----|
| `axdriver_base` | `components/axdriver_crates/axdriver_base/src/lib.rs` | `BaseDriverOps` | Common operations (name, type, IRQ) |
| `axdriver_block` | `components/axdriver_crates/axdriver_block/` | `BlockDriverOps` | Block I/O |
| `axdriver_net` | `components/axdriver_crates/axdriver_net/` | `NetDriverOps` | Network I/O |
| `axdriver_display` | `components/axdriver_crates/axdriver_display/` | `DisplayDriverOps` | Framebuffer/GPU |
| `axdriver_input` | `components/axdriver_crates/axdriver_input/` | `InputDriverOps` | Keyboard, mouse |
| `axdriver_pci` | `components/axdriver_crates/axdriver_pci/` | `PciDriverOps` | PCI bus enumeration |
| `axdriver_virtio` | `components/axdriver_crates/axdriver_virtio/` | `VirtioDriverOps` | VirtIO transport |
| `axdriver_vsock` | `components/axdriver_crates/axdriver_vsock/` | `VsockDriverOps` | VM sockets |

Actual driver implementations in `drivers/`:

| Driver | Location | Device |
|--------|----------|--------|
| simple-sdmmc | `drivers/blk/simple-sdmmc/` | SD/MMC block device |
| realtek-rtl8125 | `drivers/net/realtek-rtl8125/` | Realtek RTL8125 NIC |
| rockchip-npu | `drivers/npu/rockchip-npu/` | Rockchip NPU |
| rk3588-pci | `drivers/pci/rk3588-pci/` | Rockchip RK3588 PCIe |
| rockchip-pm | `drivers/soc/rockchip/rockchip-pm/` | Rockchip power management |
| rockchip-soc | `drivers/soc/rockchip/rockchip-soc/` | Rockchip SoC support |

Critical low-level APIs: Use `mmio-api` for MMIO/iomap and `dma-api` for DMA. Never use raw pointer casts for hardware access.

### Layer 3: OS Kernel Layers

**ArceOS:** The unikernel is a set of ~15 conditionally-compiled module crates in `os/arceos/modules/`. No kernel-user split -- the "app" links directly against modules. Feature flags on `axruntime` control which modules are active:
- Core always-on: `axhal`, `axconfig`, `axlog`, `axruntime`, `axsync`
- Optional: `axalloc`, `axmm`, `axtask`, `axdriver`, `axfs`/`axfs-ng`, `axnet`/`axnet-ng`, `axdisplay`, `axinput`, `axdma`, `axipi`

**StarryOS:** A Linux-compatible monolithic kernel built on ArceOS runtime. `os/StarryOS/kernel/src/` implements:
- `syscall/` -- Linux syscall compatibility layer (fs, io_mpx, ipc, mm, net, sync, task)
- `task/` -- Process management
- `mm/` -- Memory management with address spaces
- `file/` -- File system abstraction
- `pseudofs/` -- Pseudo filesystems (devfs, usbfs)

**Axvisor:** A type-1 hypervisor in `os/axvisor/src/`:
- `hal/` -- Hardware abstraction (host platform, memory, time, VMM)
- `vmm/` -- Virtual machine management (vcpus, device tree/config, images, IVC, timers)
- `shell/` -- Interactive debug shell

## Key Architectural Patterns

### crate_interface Pattern (Cross-Crate Trait Dispatch)

The core mechanism for breaking circular dependencies and enabling loose coupling. Defined in `components/crate_interface/src/lib.rs`.

```rust
// Define an interface in one crate (no dependency on implementors):
#[def_interface]
trait MyTrait {
    fn do_something(x: u32) -> u32;
}

// Implement in another crate:
#[impl_interface]
impl MyTrait for MyStruct {
    fn do_something(x: u32) -> u32 { x + 1 }
}

// Call from anywhere:
call_interface!(MyTrait::do_something, 42);
```

**Restrictions:** No methods with receivers (`self`); no generic parameters. This is used extensively by `axplat` (platform HAL), `axlog`, and driver dispatch.

### axplat Platform HAL Traits (def_plat_interface / impl_plat_interface)

A specialization of `crate_interface` for platform abstraction. Traits are defined in `components/axplat_crates/axplat/src/` and implementations live in per-architecture crates under `components/axplat_crates/platforms/`.

```rust
// Define a platform interface
#[def_plat_interface]
pub trait ConsoleIf {
    fn write_bytes(bytes: &[u8]);
    fn read_bytes(bytes: &mut [u8]) -> usize;
}

// Implement for a specific platform
#[impl_plat_interface]
impl ConsoleIf for Aarch64QemuVirtConsole {
    fn write_bytes(bytes: &[u8]) { /* UART write */ }
    fn read_bytes(bytes: &mut [u8]) -> usize { /* UART read */ }
}
```

The proc macros (`def_plat_interface` / `impl_plat_interface`) are defined in `components/axplat_crates/axplat-macros/src/`.

### Conditional Compilation via Features

Modules are optional and connected through Cargo feature flags. The top-level app crate selects features which flow downward to enable/disable kernel subsystems. Example from `os/arceos/modules/axruntime/Cargo.toml`:

```toml
[features]
paging = ["ax-hal/paging", "dep:ax-mm", "dep:axklib"]
multitask = ["ax-task/multitask"]
fs = ["ax-driver", "dep:ax-fs"]
net = ["ax-driver", "dep:ax-net"]
```

This means a minimal ArceOS app links only `axhal` + `axconfig` + `axlog` + `axruntime`. Adding `net` brings in the network stack transitively.

### Module Dependency Graph (ArceOS)

The canonical dependency chain, derived from `axruntime/Cargo.toml` features and `axhal/Cargo.toml` platform forwarding:

```text
axruntime (entry-point runtime)
  +-- axhal (HAL: wraps axplat traits)
  |     +-- ax-plat (trait definitions)
  |     +-- axplat-<arch> (platform impl, exactly one)
  |     +-- ax-cpu (CPU feature detection)
  +-- axconfig (build config, TOML-based)
  +-- axlog (logging via crate_interface)
  +-- axalloc [optional: alloc feature] (global allocator)
  +-- axmm [optional: paging feature] (page tables, address spaces)
  +-- axtask [optional: multitask feature] (scheduler, tasks)
  +-- axsync (Mutex, Condvar, etc.)
  +-- axdriver [optional] (driver registry + probing)
  |     +-- axdriver_base (BaseDriverOps trait)
  |     +-- axdriver_block (BlockDriverOps trait)
  |     +-- axdriver_net (NetDriverOps trait)
  |     +-- axdriver_display (DisplayDriverOps trait)
  |     +-- axdriver_virtio (VirtIO transport)
  +-- axfs / axfs-ng [optional: fs feature] (filesystem)
  +-- axnet / axnet-ng [optional: net feature] (TCP/IP via smoltcp)
  +-- axdisplay [optional] (framebuffer)
  +-- axinput [optional] (HID)
  +-- axdma [optional] (DMA engine)
  +-- axipi [optional] (inter-processor interrupts)
```

## Inter-System Relationships

### Shared Components

All three systems share these infrastructure crates:

| Component | Location | Shared By |
|-----------|----------|-----------|
| Platform traits | `components/axplat_crates/axplat/` | All three |
| Static platform impls | `components/axplat_crates/platforms/` | ArceOS, StarryOS |
| Dynamic platform impl | `platform/axplat-dyn/` | Axvisor |
| Driver traits | `components/axdriver_crates/` | All three |
| Driver implementations | `drivers/` | All three |
| Core components | `components/` (50+ crates) | All three |
| Build system | `xtask/` + `scripts/axbuild/` | All three |
| Test infrastructure | `test-suit/` | ArceOS, StarryOS |

### System-Specific Code

| System | Kernel Code | Config | Examples |
|--------|-------------|--------|----------|
| ArceOS | `os/arceos/modules/` (15 crates) | `os/arceos/modules/axconfig/` | `os/arceos/examples/` |
| StarryOS | `os/StarryOS/kernel/src/` | `os/StarryOS/configs/{qemu,board}/` | Test apps in `test-suit/starryos/` |
| Axvisor | `os/axvisor/src/` | `os/axvisor/configs/{board,vms}/` | VM configs in `configs/vms/` |

StarryOS also uses dedicated components not shared with ArceOS: `components/starry-process/`, `components/starry-signal/`, `components/starry-vm/`.

Axvisor uses virtualization-specific components: `components/axvm/`, `components/axvcpu/`, `components/axvmconfig/`, `components/axvisor_api/`, plus per-arch VCPU crates (`arm_vcpu`, `riscv_vcpu`, `x86_vcpu`, `loongarch_vcpu`).

## Configuration Mechanisms

**ArceOS:** TOML-based platform configuration via `os/arceos/modules/axconfig/`. Configs selected at build time; the `axconfig-gen` tool (`components/axconfig-gen/`) generates Rust constants from TOML.

**StarryOS:** TOML config files in `os/StarryOS/configs/{qemu,board}/`. Organized by architecture (aarch64, loongarch64, riscv64, x86_64) and target (qemu vs physical board).

**Axvisor:** Two-tier config in `os/axvisor/configs/`:
- `board/` -- Host platform configuration (9 board targets)
- `vms/` -- Guest VM definitions (50+ VM configs for ArceOS, FreeRTOS, Linux, NimbOS, RT-Thread, Zephyr guests)

## Entry Points

| System | Entry | File |
|--------|-------|------|
| ArceOS app | `main()` | `os/arceos/modules/axruntime/src/lib.rs` (calls `unsafe { main() }`) |
| StarryOS | `rust_main()` | `os/StarryOS/kernel/src/entry.rs` |
| Axvisor | `main()` | `os/axvisor/src/main.rs` |

## Architectural Constraints

- **Threading model:** Single-threaded event loop per core (no preemption in ArceOS). StarryOS adds thread model via `axtask` with round-robin scheduler. SMP supported via feature flag.
- **Global state:** Per-CPU data via `components/percpu/percpu/` (legacy `#[percpu]` macro) and `ax-percpu-macros`. Platform-specific percpu layout in `components/axplat_crates/axplat/src/percpu.rs`.
- **Memory model:** `no_std` for all kernel/hypervisor code. `alloc` crate for heap. Page tables via `components/page_table_multiarch/` (supports aarch64, riscv64, x86_64, loongarch64).
- **Circular imports:** Resolved through `crate_interface` pattern rather than direct dependency links. The `axruntime` crate is the binding point that resolves all module cross-references.
- **Target triple:** All OS bins target bare-metal: `x86_64-unknown-none`, `riscv64gc-unknown-none-elf`, `aarch64-unknown-none-softfloat`, `loongarch64-unknown-none-softfloat`.
- **Toolchain:** Rust nightly-2026-04-27, edition 2024. Locked via `rust-toolchain.toml`.

## Error Handling

- Driver layer: `DevResult<T>` = `Result<T, DevError>` from `axdriver_base`
- OS layer: `AxError` / `ax_errno` types from `components/axerrno/`
- Panic: Custom panic handler via `components/axpanic/`
- No unwinding: All kernel/hypervisor targets use `panic="abort"`

## Cross-Cutting Concerns

- **Logging:** `os/arceos/modules/axlog/` uses `crate_interface` pattern. Kernel modules implement `LogIf` to direct log output to the platform console driver. Supports log levels via `log` crate.
- **Synchronization:** `os/arceos/modules/axsync/` provides Mutex, Condvar, WaitQueue. Spinlock from `components/kspin/`. Lockdep from `components/lockdep/`.
- **Initialization order:** `axruntime` handles bootstrap: platform init early -> heap init -> driver init -> module init -> app main.
- **Build orchestrator:** `cargo xtask` via `xtask/src/main.rs` delegates to `scripts/axbuild/`.

---

*Architecture analysis: 2026-05-13*
