<!-- refreshed: 2026-05-13 -->
# Codebase Structure

**Analysis Date:** 2026-05-13

## Directory Layout

```
tgoskits/
├── components/               # 50+ standalone shared crates (git subtree managed)
│   ├── aarch64_sysreg/       # AArch64 system register access
│   ├── arm_pl011/            # ARM PL011 UART driver
│   ├── arm_pl031/            # ARM PL031 RTC driver
│   ├── arm_vcpu/             # ARM VCPU abstraction
│   ├── arm_vgic/             # ARM virtual GIC
│   ├── ax-lazyinit/          # Lazy initialization support
│   ├── axaddrspace/          # Address space management
│   ├── axallocator/          # Page/byte allocators (buddy, slab)
│   ├── axbacktrace/          # Stack backtrace support
│   ├── axconfig-gen/         # Build config TOML -> Rust codegen
│   ├── axcpu/                # CPU feature detection
│   ├── axdevice/             # Device model abstraction
│   ├── axdevice_base/        # Base device model types
│   ├── axdriver_crates/      # Driver trait definitions (8 subcrates)
│   ├── axerrno/              # Error number types
│   ├── axfs-ng-vfs/          # Next-gen VFS layer
│   ├── axfs_crates/          # Filesystem crates (devfs, ramfs, vfs)
│   ├── axhvc/                # Hypervisor call interface
│   ├── axio/                 # I/O abstractions
│   ├── axklib/               # Kernel library utilities
│   ├── axmm_crates/          # Memory management (memory_addr, memory_set)
│   ├── axpanic/              # Panic handler
│   ├── axplat_crates/        # Platform HAL (traits + 8 arch impls)
│   ├── axpoll/               # Polling/async I/O
│   ├── axsched/              # Scheduler abstractions
│   ├── axvcpu/               # Virtual CPU abstraction
│   ├── axvisor_api/          # Axvisor guest API (proc macro + runtime)
│   ├── axvm/                 # VM (virtual machine) abstraction
│   ├── axvmconfig/           # VM configuration parsing
│   ├── bitmap-allocator/     # Bitmap-based allocator
│   ├── cap_access/           # Capability access control
│   ├── cpumask/              # CPU mask bitmaps
│   ├── crate_interface/      # Cross-crate trait dispatch (core pattern)
│   ├── ctor_bare/            # Bare-metal constructor support
│   ├── fxmac_rs/             # MAC operations
│   ├── handler_table/        # Interrupt handler table
│   ├── int_ratio/            # Integer ratio types
│   ├── kernel_guard/         # Kernel guard page support
│   ├── kspin/                # Kernel spinlocks
│   ├── linked_list_r4l/      # Linux-style linked list (R4L)
│   ├── lockdep/              # Lock dependency checker
│   ├── loongarch_vcpu/       # LoongArch VCPU
│   ├── page_table_multiarch/ # Multi-architecture page tables
│   ├── percpu/               # Per-CPU data support
│   ├── range-alloc-arceos/   # Range-based allocator
│   ├── riscv-h/              # RISC-V hypervisor extension
│   ├── riscv_plic/           # RISC-V PLIC interrupt controller
│   ├── riscv_vcpu/           # RISC-V VCPU
│   ├── riscv_vplic/          # RISC-V virtual PLIC
│   ├── rsext4/               # ext4 filesystem (Rust)
│   ├── scope-local/          # Scoped thread-local storage
│   ├── starry-process/       # StarryOS process management
│   ├── starry-signal/        # StarryOS signal handling
│   ├── starry-vm/            # StarryOS virtual memory
│   ├── timer_list/           # Timer list data structure
│   ├── x86_vcpu/             # x86 VCPU (VMX/SVM)
│   └── x86_vlapic/           # x86 virtual LAPIC
│
├── os/                        # Three operating system implementations
│   ├── arceos/               # ArceOS unikernel
│   │   ├── api/              # Public API crates
│   │   │   ├── arceos_api/   # Core ArceOS API
│   │   │   ├── arceos_posix_api/ # POSIX compatibility API
│   │   │   └── axfeat/       # Feature flag aggregation
│   │   ├── modules/          # Kernel module crates (~15 crates)
│   │   │   ├── axalloc/      # Global allocator
│   │   │   ├── axconfig/     # Build configuration
│   │   │   ├── axdisplay/    # Display/framebuffer
│   │   │   ├── axdma/        # DMA engine
│   │   │   ├── axdriver/     # Driver registry and probing
│   │   │   ├── axfs/         # Filesystem (legacy VFS)
│   │   │   ├── axfs-ng/      # Filesystem (next-gen)
│   │   │   ├── axhal/        # Hardware abstraction layer
│   │   │   ├── axinput/      # Input device support
│   │   │   ├── axipi/        # Inter-processor interrupts
│   │   │   ├── axlog/        # Logging framework
│   │   │   ├── axmm/         # Memory management
│   │   │   ├── axnet/        # Networking (legacy)
│   │   │   ├── axnet-ng/     # Networking (next-gen, smoltcp)
│   │   │   ├── axruntime/    # Runtime entry point
│   │   │   ├── axsync/       # Synchronization primitives
│   │   │   └── axtask/       # Task/scheduling
│   │   ├── ulib/             # User-space libraries
│   │   │   ├── axlibc/       # libc implementation
│   │   │   └── axstd/        # Rust std-like library
│   │   ├── examples/         # Example applications
│   │   │   ├── helloworld/   # Minimal hello world
│   │   │   ├── helloworld-myplat/ # Custom platform example
│   │   │   ├── httpclient/   # HTTP client
│   │   │   ├── httpserver/   # HTTP server
│   │   │   └── shell/        # Interactive shell
│   │   ├── configs/          # Platform config TOML files
│   │   ├── tools/            # Build/debug tools
│   │   │   ├── deptool/      # Dependency graph visualizer
│   │   │   ├── bwbench_client/ # Bandwidth benchmark
│   │   │   └── raspi4/       # Raspberry Pi 4 chainloader
│   │   ├── scripts/          # Build/test scripts
│   │   └── doc/              # Documentation
│   │
│   ├── StarryOS/             # StarryOS - Linux-compatible teaching OS
│   │   ├── kernel/           # Kernel crate
│   │   │   └── src/
│   │   │       ├── config/   # Kernel configuration
│   │   │       ├── entry.rs  # Kernel entry point
│   │   │       ├── file/     # File system abstraction
│   │   │       ├── mm/       # Memory management (address spaces)
│   │   │       ├── pseudofs/ # Pseudo-filesystems (dev, usbfs)
│   │   │       ├── syscall/  # Linux syscall layer
│   │   │       │   ├── fs/   # File system syscalls
│   │   │       │   ├── io_mpx/ # I/O multiplexing
│   │   │       │   ├── ipc/  # Inter-process communication
│   │   │       │   ├── mm/   # Memory syscalls
│   │   │       │   ├── net/  # Network syscalls
│   │   │       │   ├── sync/ # Synchronization syscalls
│   │   │       │   └── task/ # Task/process syscalls
│   │   │       ├── task/     # Process management
│   │   │       ├── time.rs   # Time management
│   │   │       └── trap.rs   # Trap/exception handling
│   │   ├── starryos/         # OS binary crate
│   │   │   ├── src/          # Main binary source
│   │   │   └── xtask/        # StarryOS-specific build tasks
│   │   ├── configs/          # Platform and build configs
│   │   │   ├── qemu/         # QEMU target configs (4 archs)
│   │   │   └── board/        # Physical board configs
│   │   ├── make/             # Legacy build scripts
│   │   └── docs/             # StarryOS documentation
│   │
│   └── axvisor/              # Axvisor - Type-1 hypervisor
│       ├── src/              # Hypervisor source
│       │   ├── main.rs       # Entry point
│       │   ├── logo.rs       # Boot logo
│       │   ├── task.rs       # Hypervisor task management
│       │   ├── hal/          # Hardware abstraction
│       │   │   ├── mod.rs    # HAL module
│       │   │   ├── arch/     # Arch-specific HAL code
│       │   │   ├── impl_host.rs    # Host platform impl
│       │   │   ├── impl_memory.rs  # Memory management impl
│       │   │   ├── impl_time.rs    # Timer impl
│       │   │   └── impl_vmm.rs     # VMM impl
│       │   ├── vmm/          # Virtual machine monitor
│       │   │   ├── config.rs # VM configuration
│       │   │   ├── fdt/      # Flattened device tree
│       │   │   ├── hvc.rs    # Hypervisor call handler
│       │   │   ├── images/   # Guest image loading
│       │   │   ├── ivc.rs    # Inter-VM communication
│       │   │   ├── timer.rs  # Virtual timers
│       │   │   ├── vcpus.rs  # Virtual CPU management
│       │   │   └── vm_list.rs # VM lifecycle registry
│       │   └── shell/        # Interactive debug shell
│       │       └── command/  # Shell command implementations
│       ├── configs/          # Hypervisor configuration
│       │   ├── board/        # Host board configs (9 targets)
│       │   ├── vms/          # Guest VM definitions (50+ configs)
│       │   └── defconfig.toml # Default configuration
│       ├── scripts/          # Linker scripts, boot scripts
│       └── xtask/            # Build orchestrator for axvisor
│
├── platform/                  # Dynamic platform dispatch crates
│   ├── axplat-dyn/           # Runtime platform selection for Axvisor
│   │   └── src/              # Feature-gated per-board backends
│   ├── riscv64-qemu-virt/    # RISC-V QEMU virt platform (hypervisor variant)
│   └── x86-qemu-q35/         # x86 QEMU Q35 platform (hypervisor variant)
│
├── drivers/                   # Cross-kernel device driver implementations
│   ├── blk/                  # Block device drivers
│   │   └── simple-sdmmc/     # SD/MMC driver
│   ├── net/                  # Network drivers
│   │   └── realtek-rtl8125/  # Realtek RTL8125 2.5GbE NIC
│   ├── pci/                  # PCI subsystem drivers
│   │   └── rk3588-pci/       # Rockchip RK3588 PCIe root complex
│   ├── npu/                  # NPU (Neural Processing Unit) drivers
│   │   └── rockchip-npu/     # Rockchip NPU driver
│   ├── soc/                  # SoC support drivers
│   │   └── rockchip/         # Rockchip SoC family
│   │       ├── rockchip-pm/  # Power management
│   │       └── rockchip-soc/ # SoC base support
│   └── cpu-infer/            # CPU-based inference engine (new, WIP)
│
├── test-suit/                 # System regression tests
│   ├── arceos/               # ArceOS tests (C + Rust)
│   │   ├── c/                # C-language tests
│   │   └── rust/             # Rust-language tests
│   │       ├── display/      # Display test
│   │       ├── exception/    # Exception handling test
│   │       ├── fs/           # Filesystem tests (shell)
│   │       ├── memtest/      # Memory test
│   │       ├── net/          # Network tests (echo, http, udp)
│   │       └── task/         # Task tests (affinity, ipi, irq, lockdep,
│   │                         #   parallel, priority, sleep, tls, wait_queue, yield)
│   └── starryos/             # StarryOS tests
│
├── xtask/                     # Root build orchestrator
│   └── src/
│       └── main.rs           # `cargo xtask` entry point (delegates to axbuild)
│
├── scripts/                   # Repository management
│   ├── axbuild/              # Build system crate (shared by all OSes)
│   ├── repo/                 # Git subtree management
│   │   ├── repo.py           # Subtree pull/push script
│   │   └── repos.csv         # Component repository registry
│   └── test/                 # Test configuration
│       ├── clippy_crates.csv # Crates validated with clippy
│       └── std_crates.csv    # Crates using Rust std
│
├── docs/                      # Docusaurus developer documentation
├── models/                    # ML model files (for CPU inference)
├── CLAUDE.md                  # Project instructions for AI assistants
├── Cargo.toml                 # Workspace root (all 172 workspace members)
├── rust-toolchain.toml        # Pinned toolchain (nightly-2026-04-27)
├── rustfmt.toml               # Rust formatter config (edition 2024)
└── .vscode/                   # VS Code debug launch configs
```

## Directory Purposes

### `components/` -- Shared Component Crates
- **Purpose:** 50+ standalone Rust crates shared by all three OS systems. Each is a git subtree from its own upstream repository.
- **Contains:** Platform HAL traits, driver trait definitions, core kernel primitives (allocators, locks, page tables), virtualization support, filesystem implementations.
- **Key files:**
  - `crate_interface/src/lib.rs` -- The `#[def_interface]` / `#[impl_interface]` proc macro (core architectural pattern)
  - `axplat_crates/axplat/src/lib.rs` -- Platform HAL trait definitions (`ConsoleIf`, `InitIf`, `IrqIf`, `MemIf`, `TimeIf`, etc.)
  - `axdriver_crates/axdriver_base/src/lib.rs` -- `BaseDriverOps` trait and `DevError` type
  - `page_table_multiarch/page_table_multiarch/` -- Multi-arch page table implementation (aarch64, riscv64, x86_64, loongarch64)
  - `axvm/src/` -- Virtual machine abstraction for Axvisor
  - `starry-process/src/`, `starry-signal/src/`, `starry-vm/src/` -- StarryOS-specific process/signal/VM components

### `os/` -- Operating System Implementations
- **Purpose:** Contains all three OS kernel implementations plus their APIs, examples, and configurations.
- **Contains:** Kernel source, public API crates, user libraries, examples, build configurations.
- **Key files:**
  - `arceos/modules/axruntime/src/lib.rs` -- ArceOS entry point (bootstrap + calls `main()`)
  - `arceos/modules/axhal/Cargo.toml` -- Platform HAL dispatch (features-forward platform caps)
  - `StarryOS/kernel/src/entry.rs` -- StarryOS kernel entry
  - `StarryOS/kernel/src/syscall/` -- Linux syscall compatibility layer
  - `axvisor/src/main.rs` -- Axvisor hypervisor entry
  - `axvisor/src/vmm/` -- Virtual machine monitor logic

### `platform/` -- Dynamic Platform Dispatch
- **Purpose:** Runtime-selectable platform backends for the Axvisor hypervisor. These are distinct from the static platform crates in `components/axplat_crates/platforms/`.
- **Contains:** `axplat-dyn` (dynamic dispatch hub), `riscv64-qemu-virt` (hypervisor variant), `x86-qemu-q35` (hypervisor variant).
- **Key files:**
  - `axplat-dyn/Cargo.toml` -- Feature-gated platform backend selection
  - `axplat-dyn/src/lib.rs` -- Runtime platform dispatch

### `drivers/` -- Device Driver Implementations
- **Purpose:** Hardware-specific driver code organized by device type. These implement the traits defined in `components/axdriver_crates/`.
- **Contains:** Block, net, PCI, NPU, SoC, and CPU-infer driver implementations.
- **Key files:**
  - `blk/simple-sdmmc/` -- SD/MMC block driver
  - `net/realtek-rtl8125/` -- Realtek NIC driver
  - `npu/rockchip-npu/` -- Rockchip NPU driver
  - `soc/rockchip/rockchip-soc/` -- Rockchip SoC platform driver

### `test-suit/` -- System Regression Tests
- **Purpose:** Integration and system-level tests for ArceOS and StarryOS. Run via `cargo xtask arceos test qemu` or `cargo xtask starry test qemu`.
- **Contains:** Rust and C test applications organized by subsystem.
- **Key files:**
  - `arceos/rust/task/parallel/` -- Multi-threading tests
  - `arceos/rust/net/echoserver/` -- Network echo server test
  - `arceos/rust/fs/shell/` -- Filesystem shell test

### `xtask/` -- Build Orchestrator
- **Purpose:** Root build system. `cargo xtask` dispatches to `axbuild`.
- **Contains:** Single `main.rs` that delegates all build/test/run commands.
- **Key files:** `src/main.rs`

### `scripts/` -- Repository Management
- **Purpose:** Git subtree synchronization and test infrastructure configuration.
- **Contains:** `repo.py` for subtree pull/push, `repos.csv` tracking 60+ repositories, clippy and std crate allowlists.
- **Key files:**
  - `repo/repos.csv` -- Complete registry of all component repositories
  - `repo/repo.py` -- Subtree management script
  - `test/clippy_crates.csv` -- Crates validated with clippy
  - `test/std_crates.csv` -- Crates allowed to use Rust std
  - `axbuild/` -- Shared build system crate

### `docs/` -- Developer Documentation
- **Purpose:** Docusaurus-based documentation site.
- **Contains:** Architecture docs, getting started guides, API references.

## Key File Locations

**Workspace Configuration:**
- `Cargo.toml` -- Workspace root (172 members, all dependency versions)
- `rust-toolchain.toml` -- Pinned nightly-2026-04-27 with 4 bare-metal targets
- `rustfmt.toml` -- Formatter config (edition 2024, StdExternalCrate grouping)
- `CLAUDE.md` -- AI assistant instructions

**Entry Points:**
- `os/arceos/modules/axruntime/src/lib.rs` -- ArceOS runtime (bootstraps and calls app `main()`)
- `os/StarryOS/kernel/src/entry.rs` -- StarryOS kernel entry
- `os/axvisor/src/main.rs` -- Axvisor hypervisor entry
- `xtask/src/main.rs` -- Build system entry (`cargo xtask`)

**Core Architecture:**
- `components/crate_interface/src/lib.rs` -- Cross-crate trait dispatch macro
- `components/axplat_crates/axplat/src/lib.rs` -- Platform HAL trait hub
- `components/axdriver_crates/axdriver_base/src/lib.rs` -- Driver trait foundation
- `os/arceos/modules/axhal/Cargo.toml` -- Platform feature forwarding
- `os/arceos/modules/axruntime/Cargo.toml` -- Module feature graph

**Platform Implementations:**
- `components/axplat_crates/platforms/axplat-aarch64-qemu-virt/` -- AArch64 QEMU
- `components/axplat_crates/platforms/axplat-riscv64-qemu-virt/` -- RISC-V QEMU
- `components/axplat_crates/platforms/axplat-x86-pc/` -- x86 PC
- `components/axplat_crates/platforms/axplat-loongarch64-qemu-virt/` -- LoongArch QEMU
- `components/axplat_crates/platforms/axplat-aarch64-raspi/` -- Raspberry Pi
- `platform/axplat-dyn/` -- Dynamic platform dispatch (Axvisor)

**Build System:**
- `scripts/axbuild/src/` -- Shared build logic for all OSes
- `os/axvisor/xtask/src/main.rs` -- Axvisor-specific build tool
- `os/StarryOS/starryos/xtask/` -- StarryOS-specific build tool

**Configuration:**
- `os/arceos/configs/` -- ArceOS platform TOML configs
- `os/StarryOS/configs/qemu/` -- StarryOS QEMU configs (4 archs)
- `os/StarryOS/configs/board/` -- StarryOS physical board configs
- `os/axvisor/configs/board/` -- Axvisor host board configs (9 boards)
- `os/axvisor/configs/vms/` -- Axvisor guest VM configs (50+)

**Repository Management:**
- `scripts/repo/repos.csv` -- Complete component registry
- `scripts/repo/repo.py` -- Git subtree pull/push automation

## Naming Conventions

**Files:**
- Rust modules: `snake_case.rs` (e.g., `vm_list.rs`, `impl_host.rs`)
- Cargo packages: `kebab-case` (e.g., `ax-plat`, `starry-kernel`, `axvisor`)
- Config files: `kebab-case.toml` (e.g., `qemu-aarch64.toml`, `defconfig.toml`)
- Assembly: `snake_case.S` (e.g., `ap_start.S`, `multiboot.S`)

**Directories:**
- OS subsystems: `snake_case` within `os/` (e.g., `arceos/modules/axhal/`)
- Component groups: `snake_case` (e.g., `axplat_crates/`, `axdriver_crates/`)
- Device types: short form (e.g., `blk/`, `net/`, `pci/`, `npu/`, `soc/`)

**Crate names in Cargo.toml:**
- Unikernel modules: `ax-*` prefix (e.g., `ax-hal`, `ax-runtime`)
- Platform crates: `ax-plat-<arch>-<board>` (e.g., `ax-plat-aarch64-qemu-virt`)
- Driver traits: `ax-driver-<type>` (e.g., `ax-driver-block`)
- Components: varied (`starry-process`, `axvm`, `riscv-h`)
- OS bins: descriptive (`starryos`, `axvisor`)

## Where to Add New Code

**New ArceOS Module:**
- Primary code: `os/arceos/modules/<module-name>/`
- Register in: `os/arceos/modules/axruntime/Cargo.toml` (as optional dependency + feature)
- Tests: `test-suit/arceos/rust/<test-category>/<test-name>/`

**New ArceOS Example:**
- Implementation: `os/arceos/examples/<example-name>/`
- Add to: workspace members in root `Cargo.toml`

**New StarryOS Feature:**
- Kernel logic: `os/StarryOS/kernel/src/<subsystem>/`
- Syscall impl: `os/StarryOS/kernel/src/syscall/<category>/`
- Tests: `test-suit/starryos/<test-name>/`

**New Axvisor Feature:**
- Hypervisor logic: `os/axvisor/src/<module>/`
- VM config: `os/axvisor/configs/vms/<guest>.toml`
- Board support: `os/axvisor/configs/board/<board>.toml`

**New Driver:**
- Implementation: `drivers/<type>/<driver-name>/`
- Register in: `Cargo.toml` workspace members + `[workspace.dependencies]`
- Trait impl: implement `BaseDriverOps` + device-specific trait from `components/axdriver_crates/`

**New Platform Support:**
- Static (ArceOS/StarryOS): `components/axplat_crates/platforms/axplat-<arch>-<board>/`
- Dynamic (Axvisor): Add backend module to `platform/axplat-dyn/src/` + feature flag

**New Shared Component:**
- Implementation: `components/<crate-name>/`
- Register in: `Cargo.toml` workspace members + `[workspace.dependencies]`
- Add to: `scripts/repo/repos.csv` if managed via git subtree

**New Test:**
- ArceOS Rust: `test-suit/arceos/rust/<category>/<test-name>/`
- ArceOS C: `test-suit/arceos/c/<test-name>/`
- StarryOS: `test-suit/starryos/<test-name>/`

## Special Directories

**`components/` subdirectories:**
- Purpose: Shared infrastructure crates, each is a git subtree from its own repository
- Generated: No
- Committed: Yes (but subtree history is managed by `repo.py`, not direct git)

**`os/arceos/tools/deptool/`:**
- Purpose: Generates dependency graphs for ArceOS module analysis (supports D2 and Mermaid formats)
- Generated: No (tool source)
- Committed: Yes

**`os/axvisor/configs/vms/`:**
- Purpose: Device tree source (`.dts`) and TOML files defining guest VM configurations
- Generated: `.dts` files are hand-written, `.dtb` compiled at build time
- Committed: Yes

**`test-suit/` output directories:**
- Purpose: Build artifacts for test binaries (e.g., `test-suit/arceos/c/*/build_*/`)
- Generated: Yes (build output)
- Committed: No (in `.gitignore`)

**`models/`:**
- Purpose: ML model files for CPU-based inference engine
- Generated: No (model files)
- Committed: Not yet (untracked, new feature)

---

*Structure analysis: 2026-05-13*
