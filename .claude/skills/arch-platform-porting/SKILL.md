---
name: arch-platform-porting
description: Add, adapt, debug, or review architecture/platform support for ArceOS, StarryOS, Axvisor, someboot, dynamic UEFI platform boot, SMP startup, QEMU boot configs, target JSON files, axbuild arch mapping, axcpu trap/context code, axplat-dyn, somehal, and LoongArch/x86/aarch64/riscv platform bring-up issues.
---

# Arch Platform Porting

Use this skill when adding or fixing an architecture, switching QEMU cases to dynamic UEFI platform boot, enabling SMP in someboot, debugging early boot hangs, or validating ArceOS/StarryOS/Axvisor on a new arch/platform path.

For detailed pitfalls and debugging notes from the LoongArch dynamic UEFI/SMP bring-up, read `references/boot-debugging.md` when the task touches early boot, trap vectors, MMU, SMP, UEFI exit, or Axvisor LVZ QEMU.

Current Axvisor LoongArch QEMU bring-up uses the dynamic UEFI platform path. The host AxVisor boots through LoongArch OVMF, and Linux guests boot through guest UEFI with the kernel/rootfs read from the AxVisor runtime rootfs and the local OVMF firmware captured at build time.

## First Pass

1. Identify the layer that is changing: target spec, axbuild, test-suit config, someboot, axcpu, axplat-dyn/somehal, device driver, or OS config.
2. Inspect the closest working architecture first. For dynamic UEFI paths, compare with x86_64 before inventing new behavior.
3. Trace the full boot contract from QEMU args to kernel entry. Do not assume a QEMU config change is enough if the firmware, target ABI, loader, and runtime platform disagree.
4. Prefer `cargo xtask` flows for ArceOS, StarryOS, and Axvisor. If a special QEMU/container setup needs raw commands, inspect the xtask path and match its arguments.
5. Keep temporary debug markers out of the final patch unless the user explicitly asks to retain them.

## Porting Checklist

- **Target and toolchain**: add or verify `scripts/targets` specs, target triple, panic strategy, relocation model, code model, ABI, soft-float setting, musl/std support, linker, objcopy, and `rust-src` availability.
- **RISC-V global-pointer contract**: `gp` is always the canonical psABI global pointer and is never a CPU ID or per-CPU anchor. Current dynamic PIE targets keep `--no-relax` so the final-high entry can explicitly rebuild `__global_pointer$` before Rust or global-data access; do not remove it without the random-load-bias relocation tests proving every early path safe.
- **Build system**: wire arch/target mapping in `scripts/axbuild`, dynamic platform defaults, feature propagation, kernel format conversion, UEFI/to-bin behavior, rootfs handling, and per-OS test discovery.
- **QEMU and firmware**: verify QEMU binary, machine type, CPU, SMP count, pflash/OVMF files, serial console, disk/rootfs device, `-snapshot`, debug flags, timeout, and success/fail regexes.
- **someboot arch layer**: implement or audit entry, relocation, BSS clearing, stack setup, memory map parsing, paging, trap vectors, timer, IRQ, power, SMP, and address translation.
- **CPU runtime**: update `components/axcpu/src/<arch>` for trap entry, context switch, user/kernel context, syscall return path, FP/SIMD state, and per-CPU assumptions.
- **Platform bridge**: update `platforms/axplat-dyn`, `platforms/somehal`, platform config, memory regions, IRQ routing, timer source, power operations, and CPU boot operations.
- **Runtime platform identity**: dynamic platform names should be discovered in `someboot`/`somehal` from firmware data, then exposed through `axplat-dyn` and `ax_plat::platform::platform_name()`. Keep `ax-hal` as a forwarding layer for platform identity, and keep static platforms returning `config::PLATFORM`.
- **Runtime IRQ ownership**: ArceOS runtime IRQ traps are owned by `ax-cpu` and dispatched through `ax_hal::irq::handle_irq(raw_vector)`, which immediately wraps the CPU trap entry as `TrapVector`. `somehal` must stay OS-free and expose controller transactions through `somehal::irq::begin_irq(raw_vector) -> ActiveIrq`; `ActiveIrq::id()` returns the resolved `IrqId`, and `ActiveIrq` is held while `axplat-dyn` dispatches the IRQ and its `Drop` performs the architecture-specific EOI/complete. Do not reintroduce `_someboot_handle_irq` or `#[somehal::irq_handler]` as runtime dispatch glue.
- **Runtime IRQ initialization order**: dynamic platforms initialize boot IRQ state through `ax_hal::irq::init_boot_irqs(cpu_id)` before registering runtime IRQ handlers or probing normal devices. `rdrive::ProbeLevel` remains the coarse lifecycle boundary, and `ProbePriority` is the ordering source inside `PreKernel`: clocks first, then interrupt controllers, timer sources, MSI parent controllers, and only later normal early devices. For FDT, same level/priority matches must keep device-tree order; interrupt-controller nodes additionally follow parent-before-child ordering similar to Linux `of_irq_init()`, with sibling controllers preserving DT order. Do not add arch-specific ad hoc probe calls in `axruntime` when a priority barrier can express the same dependency.
- **Scheduler-online initialization order**: once a CPU can schedule, do not use raw `spin::Once` or `spin::LazyLock` for state that competing tasks may initialize. A raw Once owner can publish `Running`, be preempted, and then be waited on by a same-CPU replacement holding an IRQ/preemption guard. Use `ax_kspin::PreemptOnce` / `PreemptLazy`, complete lazy state before publishing dependent services, and publish the service before making its worker threads runnable. Keep first allocation and worker spawning outside `SpinNoIrq` service guards.
- **Runtime IPI identity**: dynamic platforms expose the runtime IPI IRQ as a typed `IrqId` through `somehal::irq::ipi_irq()`, `axplat-dyn`, and `ax_hal::irq::ipi_irq()`. Do not route dynamic runtime IPI registration through `ax-config`; on RISC-V the IRQ is the flagged supervisor software interrupt cause in the CPU-local domain, not bare PLIC source `1`.
- **Runtime IPI transport**: publish an immutable dense logical-CPU to firmware/hardware-ID table before any CPU becomes online, then resolve every runtime target from that O(1) table without reparsing firmware, allocating, logging, or taking controller-discovery locks. Carry destinations as `CpuIpiTarget` and return `IpiSendStatus::{Success, Retry, Invalid}`. The lowest public hardware sender must borrow `IrqGuard` across current-CPU observation, validation, and commit so split xAPIC ICR writes cannot be nested. Order inbox/event publication before the hardware doorbell with the architecture's store/device barrier. A software broadcast must preflight every permanently invalid destination before its first commit; prefer independent per-target generations when partial transient failure needs independent retry.
- **Runtime CPU limits**: treat generated `CPU_CAPACITY`/`SMP` as a build-time capacity for const generics, per-CPU arrays, and linker/percpu layout only. Actual online/usable CPU count must flow through `ax_hal::cpu_num()`, which caps the platform-discovered count by capacity.
- **Starry SMP build contract**: always compile the top-level Starry package with its `smp` capability, including explicit SMP1 runs and board build files that omit `max_cpu_num`. Use `SMP` only to select build capacity/runtime exposure. Keep this policy in the Starry axbuild adapter rather than the shared no-std preparation, because ArceOS C builds use the same generic preparation with different feature semantics.
- **IRQ namespace rules**: keep CPU trap vectors, platform `IrqId { domain, hwirq }`, firmware sources (`IrqSource::AcpiGsi`, `IrqSource::AcpiGsiRoute`, explicit `IrqSource::ControllerLine`, and driver binding metadata such as `BindingIrqSource::FdtInterrupt`), controller-local hardware lines (`HwIrq`), and guest GSI/vector values in separate namespaces. New runtime IRQ registrations must use `IrqId`, not `usize`; legacy `IrqNumber(raw)` is only for static or still-unmigrated platform boundaries and must live in OS/HAL-facing layers such as `ax-plat`, `ax-hal`, or `axklib`, not `irq-framework` or `somehal`. `irq-framework` owns generic registry, affinity, execution, and boxed callback dispatch semantics; platform rebase work must preserve `BoxedIrqHandler`, `IrqExecution`, and `IrqRequest::new_boxed` while adapting the surrounding platform code to `IrqId`. `LEGACY_IRQ_DOMAIN` and `CPU_LOCAL_IRQ_DOMAIN` remain fixed compatibility domains, while dynamic `somehal` external controller domains such as GIC, PLIC, IOAPIC, EIOINTC, and PCH-PIC are allocated at controller probe time and must be reached through `alloc_irq_domain`, `domain_by_kind`, `domain_by_owner`, or `domain_is_kind`, not by constructing fixed numeric controller domains in dynamic-platform code. Do not derive a host IRQ with arithmetic such as `0x20 + gsi`, `PCI_INTX_VECTOR_BASE + gsi`, or by subtracting a trap-vector base in Axvisor/device code. Resolve firmware/device descriptions with `ax_hal::irq::resolve_irq_source(...)` / platform resolver and register the returned `IrqId`. When ACPI supplies trigger/polarity/controller metadata, carry it as `IrqSource::AcpiGsiRoute` instead of flattening it to a bare `AcpiGsi`, because PCI INTx routes may use a low GSI with non-ISA level/low semantics. Likewise, FDT device bindings should carry the raw interrupt specifier plus its controller owner in `BindingIrqSource::FdtInterrupt` until the OS/platform layer can resolve that owner to a controller domain and configure it; do not expose parentless FDT cells from `irq-framework` or configure a controller in generic driver probe merely to obtain a legacy number. `rdif_intc` controllers must expose fallible `translate_fdt` / `translate_acpi` methods that return controller-local hardware line and trigger metadata; the registering platform allocates or looks up a domain owner entry for the concrete `rdrive::DeviceId`, passes that domain to `rdif_intc::Intc::new(domain, driver)`, and the wrapper combines that domain with the local `HwIrq` before `configure` / `configure_acpi` programs trigger, polarity, vector, or mask state. Platform `irq_set_enable` and `irq_set_affinity` paths must route by the incoming domain's registered owner/kind and return an error on missing controllers, lock failures, unsupported affinity, or backend/type mismatches instead of silently no-oping. Empty, malformed, out-of-range, or unsupported firmware specifiers must return `IrqError` instead of IRQ 0, a base vector, or a guessed legacy number. If an FDT PCI host bridge preconfigures a controller-level legacy INTx route, store that route as a native `BindingIrq` source (plus any temporary raw compatibility value) and let child endpoints reuse it before falling back to PCI `interrupt-map` parsing.
- **Domain expectations**: x86 LAPIC timer and IOAPIC are distinct domains, so trap vector `0x20` is not `AcpiGsi(0)`. On aarch64, GIC INTID is the `HwIrq` within the GIC domain. On riscv64, PLIC source is the `HwIrq` within the PLIC domain. On loongarch64, EIOINTC and PCH-PIC must remain separate domains. A platform that cannot resolve an `IrqSource` must return `IrqError::Unsupported` instead of guessing a numeric IRQ.
- **x86 QEMU IRQ contract**: the dynamic x86 path targets modern QEMU `q35` with ACPI/MADT, Local APIC or x2APIC, IOAPIC, and PCI INTx routing. Do not add 8259/PIC fallback, i440fx-specific IRQ assumptions, non-ACPI IRQ probing, raw GSI enable bypasses, or vector arithmetic outside the IOAPIC controller. LAPIC/x2APIC owns timer, IPI, EOI, and spurious handling; `X86IoApicIntc` owns external GSI route state, vector conflict checks, trigger/polarity, mask, and affinity updates through `rdif_intc::Intc`. x2APIC paths must preserve full `u32` APIC IDs for CPU-local operations, while xAPIC and IOAPIC physical destinations must reject APIC IDs that cannot be encoded without truncation.
- **LoongArch QEMU IRQ contract**: the dynamic LoongArch path targets QEMU `virt`/LS7A-style firmware routing through CPU-local timer/IPI lines, EIOINTC, and PCH-PIC. `somehal::begin_irq(raw)` receives the CPU interrupt line from `ESTAT.IS`, not an ACPI GSI or PCI vector; only the timer line, IPI line, and EIOINTC cascade line may enter runtime dispatch. EIOINTC owns claim/complete of external vectors, while PCH-PIC owns PCH input state, ACPI trigger/polarity configuration, mask state, and route memory through its `rdif_intc::Intc` lock. Do not infer PCH-PIC input by subtracting `PCI_INTX_VECTOR_BASE`, do not treat ACPI `route.vector = PCI_INTX_VECTOR_BASE + gsi` as the EIOINTC hardware vector, and do not dispatch unknown CPU-local interrupt lines as PCH-PIC IRQs.
- **RISC-V QEMU IRQ contract**: the dynamic RISC-V path targets QEMU `virt` firmware routing through CPU-local supervisor timer/software/external interrupt causes and one PLIC domain. `somehal::begin_irq(raw)` receives `scause.bits()`, not a PLIC source number; only S-timer, S-soft, and S-ext are runtime CPU-local causes. PLIC source IDs are controller-local `HwIrq`s and may only be produced by FDT translation or by claiming the PLIC after an S-ext trap. Do not dispatch a bare source number as a trap, do not treat PLIC source 0 as valid, and route PLIC enable through the registered `rdif_intc::Intc` controller instead of bypassing the rdrive lock.
- **Runtime console selection**: Dynamic platforms expose the firmware-selected hardware console through `somehal::console_device_id()` and `ax_hal::console::device_id()`. The value is `Result<rdrive::DeviceId, ConsoleDeviceIdError>` derived from bootargs `console=`, ACPI SPCR, or FDT `stdout-path`; static platforms return `Err(NotSpecified)`. OS code such as Starry should match `Ok(id)` against probed serial devices, use `ttyS0` as the Linux-style hardware-console fallback only for `Err(NotSpecified)`, and leave `/dev/console` unbound (`ENODEV`) for non-hardware console selections, unmatched selected hardware devices, or when no serial console TTY exists. Do not reparse FDT or bootargs in the tty layer.
- **Runtime console ownership**: use `ax_hal::console::prepare_runtime_output_handover()` before an interrupt-driven runtime touches the firmware-selected UART. The platform uses one global, migration-safe generation token and an atomic phase/access-count word to wait for every in-flight early register access and change early I/O from `Active` to `Paused`; new boot writes are consumed and reads stop while the runtime starts in ordinary task context. Commit the token only after device and OS IRQ activation succeeds. On failure, first call the portable serial control endpoint's `quiesce_to_polling()` to mask device IRQs and enter polling state without disabling or reconfiguring the UART, then let the token drop to restore early access. If quiescing fails, commit the platform token to keep early access permanently suppressed and report a fatal invariant—never restore a possible second register owner. Never hold a context-aware lock or a long IRQ-off guard across UART startup, IRQ registration, logging, or user-task publication; a `!Send` marker is not a CPU pin and must not be used to fake this requirement. The hardware console must have one register owner at every point; a claimed-state check without an in-flight access count is insufficient.
- **Dynamic firmware devices**: for `rdrive` ACPI probes, real non-empty ACPI ID lists enumerate namespace `Device` nodes and expose `_CRS` memory, I/O port, and IRQ resources through `AcpiInfo`; empty ID lists or synthetic root IDs are reserved for root-table style callbacks.
- **Page tables and memory**: check PTE flags, huge page support, direct map, kernel high map, MMIO map, TLB/cache barriers, and early `phys_to_virt` behavior before MMU state is fully recorded.
- **Firmware address shape**: if firmware tables expose CPU-visible aliases such as LoongArch DMW addresses, canonicalize them through the architecture boundary before handing them to FDT memory setup, early console, or MMIO backends. Do not hide arch masks in generic `mem`/`common` helpers or duplicate them in drivers.
- **Runtime MMIO mapping contract**: keep `phys_to_virt` / `virt_to_phys` scoped to RAM direct-map translation. Device resource mapping must enter through `ax-mm::iomap()`, which asks `ax_hal::mem::prepare_iomap()` for an arch/platform decision before falling back to page-table-backed device mappings. Architecture-specific aliases such as LoongArch uncached DMW belong behind `someboot::ArchTrait::ioremap_device()`, not in `ax-mm` or drivers.
- **Drivers and rootfs**: check PCI command bits, MMIO/iomap, DMA address width, virtio transport, block device visibility, rootfs patching, and console/input feature flags.
- **OS configs and test cases**: update ArceOS, StarryOS, and Axvisor configs only for validated architectures. Keep `qemu-<arch>.toml` runtime config separate from `build-*.toml`.

## someboot Must-Haves

- Preserve the firmware entry ABI. UEFI entry carries `image_handle` and `system_table`; direct boot paths use different arguments.
- Establish an early console before risky transitions, then ensure a post-UEFI/post-MMU console path exists without Boot Services.
- Capture the memory map and kernel image physical range before address translation helpers depend on them.
- Treat relocated symbols carefully. After relocation or high-half switch, use runtime-safe symbol address helpers instead of raw compile-time addresses.
- Both x86 UEFI and direct/non-UEFI entry must apply the image load bias before
  the first Rust call, GOT access, or per-CPU initializer-table access. They
  converge on the same final-high `prime_entry`; a direct boot path is not a
  fixed-address exception.
- On AArch64, pass EL transition state into the post-relocation entry path when it must be kept in Rust globals; do not write relocatable statics before relocation has been applied.
- Clear BSS exactly once and after preserving any entry data that lives there.
- On LoongArch OVMF, capture the EFI FDT configuration table as well as ACPI RSDP for firmware-described devices, but do not rediscover RTC in someboot/somehal through those tables. The dynamic UEFI RTC path should first use the UEFI Runtime Service `GetTime`; LS7A RTC nodes such as `loongson,ls7a-rtc` and ACPI `LOON0001` belong to the `ax-driver` fallback path when firmware RTC is unavailable.
- Allocate and align boot stack, per-CPU areas, secondary stacks, boot arguments, and page tables before enabling SMP.
- Install trap vectors before enabling interrupts, timer interrupts, MMU faults, or secondary CPU execution.
- On x86 QEMU, do not trust CPUID timing leaves unless the reported TSC frequency is plausible; some virtual CPU combinations expose invalid zero or tiny values. Prefer a trusted hypervisor timing leaf, then CPUID timing data, then PIT-based TSC calibration before falling back to processor base frequency.
- On x86 QEMU, initialize LAPIC/x2APIC once and keep APIC IDs as firmware IDs, not logical CPU indices. Use x2APIC MSRs when x2APIC is enabled, bound IPI delivery waits, reject xAPIC AP startup/IPI destinations above 255, and keep external IOAPIC INTx programming in the runtime `X86IoApicIntc` path instead of someboot or HAL bypass helpers.
- On AArch64, keep the someboot `hv` feature scoped to the EL2 kernel path. For non-`hv` EL1 boot, choose the EL1 arch timer at runtime from the boot EL: use CNTP when EL2 is available and CNTV when EL2 is unavailable, and keep the FDT timer interrupt index consistent with the selected mode.
- On AArch64 secondary entry, preserve the CPU metadata pointer explicitly across MMU-enable and EL-transition helpers. Naked asm should consume the helper return register instead of assuming scratch registers survive Rust calls.
- Build page tables for identity/firmware access, direct map, kernel high map, MMIO, and per-CPU data as the arch requires.
- Flush TLB/cache and use architecture barriers around page table writes, boot argument writes, and secondary CPU release.
- Treat hardware MMU enablement, direct-map/kernel-space addressability, and final kernel relocation as separate states. Generic relocation detection should use the final `VM_LOAD_ADDRESS`, not the broader arch kernel/direct-map range; for example AArch64 `hv` builds can use `PAGE_OFFSET = 0`, and LoongArch DMW can make RAM addressable before execution reaches the final high mapping.
- On AArch64, keep the SCTLR.M enable to relocated-entry branch window free of UART logging. Address helpers must not switch to relocated addresses while still executing on the pre-relocation path.
- On LoongArch, do not treat the DMW direct-map window as final kernel relocation. Address helpers may use DMW for early direct mapping, but relocated-kernel checks should only become true after execution reaches the final `VM_LOAD_ADDRESS` high mapping.
- After `ExitBootServices`, do not call UEFI Boot Services. Retry only through the correct memory-map-key sequence before exit.

## SMP Bring-Up Rules

1. Discover enabled CPUs from firmware data and keep firmware IDs separate from logical CPU IDs.
2. Bound-check CPU indices and avoid assuming hart/apic/mpidr/cpuid values are dense.
3. Treat the firmware CPU count as untrusted layout input. Calculate every
   metadata, stack, and per-CPU-data offset with checked add/multiply/alignment,
   validate the final Rust allocation layout, and reject overflow before
   changing the firmware memory map, allocating storage, or publishing the
   runtime CPU count.
4. Prepare one boot argument block per secondary CPU with stack, page table, kernel entry, per-CPU base, and logical ID.
5. Flush boot arguments and page tables before `cpu_on`.
6. In the secondary path, initialize arch address windows and stack, then make the selected platform's value-only `CpuBindingV1` the first `somehal` operation. The binding must install and verify the initialized `CpuAreaHeader` through `ax-cpu-local` before `arch::secondary_init`, GIC/timer setup, `ax-kspin`, logging, or any other code that can inspect CPU-local state. The platform entry is the only CPU-area binder; generic runtime code may publish its own fields but must not replace the hardware anchor.
7. After the architecture anchor is bound but before generic OS runtime state is initialized, use cached controller fast paths for interrupt and timer setup through `somehal::irq::init_secondary_boot_irqs(cpu_id)`; do not take `rdrive`, IRQ-domain, or generic route locks from that window.
8. Publish that CPU's controller-local IPI target only after its private interrupt interface is initialized. Treat missing redistributor/target identity, duplicate GICv2 target bits, or unencodable GICv3 affinity as a bring-up failure; do not add the CPU to the online mask and hope the first IPI repairs it.
   For GICv2, scan all banked SGI/PPI `ITARGETSR` bytes instead of assuming target bit 0. A uniprocessor implementation may legally expose those fields as RAZ/WI, so model that case as an implicit local target and leave SPI target registers untouched. SMP requires one unique explicit target bit per CPU, and distributor initialization must route SPIs to the discovered boot-CPU bit rather than a hard-coded `0x01`.
9. Debug secondary failure with physical-address markers first; serial logging may not work until the secondary has its own mapping and trap state.

## CPU-Local and Task-Register Ownership

Keep physical-CPU identity, kernel-task TLS, and user register state in separate
hardware registers and separate Rust types. `ax-cpu-local` is the only crate
allowed to contain the small architecture register primitives; `ax-percpu` and
its proc macro perform only layout, offset, and ordinary Rust pointer arithmetic.

- Every runtime per-CPU area starts with a feature-invariant, three-cache-line
  `CpuAreaPrefixV2`: an immutable `CpuAreaHeader`, a CPU-owned runtime anchor
  containing the current-thread slot and trap/stack pointers, and a permanent
  `BootThreadHeader`. The header publishes `self_base`, logical CPU index,
  generation, cookie, ABI version, register mode, and host exception level; it
  never stores a template relocation. Remote access derives an area in O(1)
  from the frozen layout and must not call a platform base callback.
- `#[def_percpu]` reserves `MaybeUninit<StorageType>` only. It must register a
  typed initializer retained in `.ax_percpu.init`; never copy an initialized
  Rust object from a template. Early someboot allocates raw areas only. After
  the final high mapping and load-bias relocation are complete, `prime_entry`
  validates every relative descriptor, constructs every prefix and typed value
  exactly once, publishes the frozen layout with Release ordering, and only
  then exposes per-CPU metadata or enters `__someboot_main`.
- Map CPU-local RAM with the same coherent normal-memory cacheability and
  shareability attributes as every other alias of the same physical pages.
  AArch64 must not mark the dedicated per-CPU alias non-shareable: scheduler
  inboxes, allocator ownership, diagnostics, and remote access intentionally
  cross CPU boundaries. After final-high typed construction publishes the
  layout, late boot may clean the separate `PerCpuMeta` records needed by AP
  trampolines, but it must never clean-invalidate the complete live CPU-data
  and stack allocation again.
- Treat the header's 64-byte alignment as a minimum, never as a maximum symbol
  alignment. Linker scripts must retain `.ax_percpu.align`, derive the template
  requirement with `MAX(64, ALIGNOF(.percpu))`, and apply it to both the runtime
  base and every area stride. Platform allocators must consume that published
  requirement; a page boundary alone is insufficient for over-aligned Rust
  objects.
- `CpuPin` proves only that the caller cannot migrate. Safe current-area access
  additionally requires `ax_percpu::BoundCpuPin`, obtained by validating the
  live raw anchor against the installed layout, stride, index, generation, and
  cookie while borrowing the migration pin. `PreemptGuard`, `IrqGuard`, and
  their combined form may lend the `CpuPin`; they must not manufacture the
  stronger bound proof. Only early boot, trap entry, context-switch glue, and
  the lock runtime may use explicitly documented unchecked access. A safe
  accessor must not return a reference that can outlive its `BoundCpuPin`.
- Select exactly one final-image register mode. `LinuxCurrent` is mandatory for
  StarryOS and any ArceOS image with userspace: x86 reads current from the
  kernel-GS CPU slot, AArch64 uses `SP_EL0=current` with TPIDR_EL1/EL2 as the
  CPU base, RISC-V uses `tp=current` with the current header carrying the CPU
  base, and LoongArch uses `tp=current` with `r21`/KS3 as the CPU base.
  `UnikernelTls` is selected by Axvisor and is allowed for ArceOS images without
  host userspace: FS_BASE, TPIDR_EL0, or `tp` holds kernel TLS and current is
  read from the CPU slot; RISC-V then keeps the CPU prefix in `sscratch`. Cargo
  `tls` selects this image mode but must never change prefix,
  thread-header, or `TaskContext` layout. Reject `uspace + tls` at build time.
- ArceOS, StarryOS, and Axvisor production images on all four architectures use
  the dynamic `someboot -> somehal -> axplat-dyn` path and may be loaded at an
  arbitrary slide. Keep the fixed header at template offset zero, let ordinary
  Rust `addr_of!` materialize both loaded template boundaries, and immediately
  reduce generated variable addresses to checked offsets from that loaded
  origin. Runtime access is always `area_base + template_offset`; do not expose
  symbol VMAs or hand-code absolute symbol relocations in `ax-cpu-local` or the
  proc macro. The platform ABI publishes only runtime base, stride, count, and
  CPU binding. It must not pass a link VMA or image slide into `ax-percpu`.
- On RISC-V, the naked firmware entry captures `a0` in `CpuBootInfoV1`; an early
  scratch pointer may name that record, never the raw hart ID. After binding,
  `LinuxCurrent` follows Linux entry semantics: kernel `tp` names the pinned
  current-thread header and kernel `sscratch` is zero; user execution places
  kernel `tp` in `sscratch`, trap entry exchanges it with user `tp`, and kernel
  entry clears `sscratch` again. `UnikernelTls` instead keeps the CPU prefix in
  `sscratch` and TLS in `tp`. Every high-address trampoline rebuilds canonical
  `__global_pointer$`; `gp` is never a CPU ID or per-CPU anchor.
- On x86_64, a user exception may align the TSS-provided RSP down to 16 bytes
  before constructing its hardware frame. Any task-owned `UserContext` whose
  trap-frame end is published as `TSS.RSP0` must therefore be explicitly
  16-byte aligned, and the trap-frame size must preserve that alignment. Keep
  compile-time layout assertions beside the type; do not rely on the enclosing
  allocation or align only the value written by assembly, because either can
  separate the hardware frame from its saved kernel continuation.
- LoongArch KS allocation is fixed: KS0 is trap stack, KS1/KS2 are trap
  temporaries, KS3 mirrors the direct per-CPU area base, and KS4/KS5 belong to vCPU host
  stack/temporary state. User trap entry saves user `r21` before loading the
  KS3 kernel value; kernel return never restores a frame's `r21`.
- Each runtime context pins one independent `CurrentThreadHeader`. It gains its
  generation-bearing identity exactly once before `New` can become `Ready`;
  scheduler migration updates its CPU base/index/epoch only while the context
  is off CPU, then publishes the binding with Release ordering. `TaskContext`
  has a feature-invariant current-header and kernel-TLS slot (zero when TLS is
  disabled), plus callee state, stack and optional FP/SIMD. It never contains a
  CPU anchor or address-space register. Publish the next CPU slot/header, then
  install next current/TLS in the final naked switch window; execute no old-task
  Rust helper, hook, log, FPU helper, or MM helper afterward.
- An AArch64 vCPU must treat `TPIDR_EL0` as shared host-task/guest state. Save
  host `TPIDR_EL0` before guest entry, install the guest value only in the final
  assembly window immediately preceding `eret`, and on VM exit save the guest
  value then restore the host value before any Rust helper, log, or exit handler
  can run. Derive both slots with `offset_of!`; generic system-register
  save/restore helpers must not touch `TPIDR_EL0`.
- AArch64 LinuxCurrent lends `SP_EL0` to user SP only across the final `eret`
  window. Lower-EL trap entry must save user `SP_EL0`/`TPIDR_EL0`, reload the
  pinned current header from the TPIDR_EL1/EL2 CPU-prefix slot, and restore
  `SP_EL0=current` before returning to Rust. Select `arm-el2` only in the
  AArch64 hypervisor final-image dependency; a generic `hv` feature must not
  contaminate EL1 or non-AArch64 dependency graphs.
- AMD SVM `VMLOAD` installs guest FS/GS and therefore crosses both host
  CPU-local and task-local ownership boundaries. Keep guest `VMLOAD -> VMRUN ->
  VMSAVE -> host VMLOAD` in one naked assembly window with no Rust call,
  return, logging, or helper between the two VMLOAD instructions. Derive the
  world-switch frame offsets with `offset_of!` and restore host FS/GS before
  returning to Rust.
- Trap assembly enters Rust through an `unsafe extern "C"` raw-pointer ABI.
  Distinguish kernel and user restore paths: kernel restore preserves the live
  CPU anchor, while user restore round-trips user `gp`/`r21`/`tp` explicitly.

## Scheduler Runtime Bring-Up

When the OS uses the OS-independent `ax-task` scheduler, keep scheduler objects
out of architecture and platform crates. The OS runtime owns one pinned global
`TaskSystem` plus one pinned `CpuLocal` allocation in each per-CPU slot.

- On the primary CPU, let the platform entry bind the architecture per-CPU register first, then
  the runtime IRQ/preemption nesting state, allocator/MM/HAL services, the
  `TaskSystem`, and the primary `CpuLocal`. Register timer and scheduler-IPI
  delivery only after those objects are address-stable; enable interrupts last.
  Once the primary scheduler is online, start the permanent ax-task task-work
  service before device initialization, application entry, or any subsystem that
  can publish deferred callbacks or spawn dependent workers.
- On a secondary CPU, verify its platform-installed architecture per-CPU register and initialize runtime
  guard state before allocating or publishing `CpuLocal`. Do not add the CPU to
  the scheduler online mask until its bootstrap context, timer and IPI receive
  path are ready.
- Only an `UnikernelTls` image installs a temporary task-owned bootstrap TLS area as
  soon as the allocator and architecture per-CPU register are ready. Platform
  late-init, exception reporting, logging, and `std` may touch TLS before the
  scheduler exists. Never use this TLS register as a CPU ID or per-CPU anchor.
  After `install_bootstrap_thread` commits ownership, publish
  its execution context and long-lived TLS together, switch the hardware thread
  pointer, and only then release the temporary area. Every later context switch
  must verify that the scheduler's `previous` context equals the context
  currently published by that CPU.
- Treat generated `CPU_CAPACITY` as allocation capacity only. Placement and
  Deadline root-domain admission must use the scheduler's published online
  mask, whose size remains capped by `ax_hal::cpu_num()`.
- Timer and scheduler-IPI hard handlers must only acknowledge hardware, publish
  bounded accounting/inbox work, and set sticky `need_resched`. Drain remote
  work, run OS switch hooks and change contexts at the IRQ-return scheduler safe
  point with local IRQs disabled and no runqueue lock held.
- The general `ax-ipi` callback IRQ follows the same split: hard IRQ only marks
  per-CPU deferred work. Execute and drop `Box`/`Arc` callbacks after controller
  EOI and hard-IRQ marker removal, in a fixed-size IRQ-return batch. Re-raise a
  self IPI when work remains; never drain an unbounded callback queue in one
  interrupt or free callback storage in hard IRQ.
- Treat scheduler and callback IPIs as generation-owned doorbells. Publish work
  before claiming/sending; let a sender clear only its exact generation with
  CAS, so a stale `Retry` cannot erase a newer claim. `Retry` must enter a
  preallocated persistent per-target retry set and make bounded progress even
  without a new producer. A permanent `Invalid` scheduler target is a fail-stop
  invariant, not an infinite WFI gate. Any IPI observed after publication may
  serve as the receive doorbell; acknowledge the generation only at the owner
  safe point where the published reason is consumed.
- Service only a bounded callback-retry batch before each idle scheduler pass.
  Persistent transport `Retry` must reject the final WFI but must never skip
  `schedule_current_cpu`, because an independent remote task wake may already
  have made local work runnable. Multicast publication may hold one
  `PreemptGuard` to stabilize the source CPU, but each queue operation and
  hardware kick must use its own short IRQ-off section rather than masking IRQs
  across an O(CPU-count) loop.
- Route every scheduler timer update through one runtime-owned one-shot mux that
  programs the earlier of the periodic tick and task deadline. A task deadline
  must never directly overwrite an earlier periodic deadline or bypass the
  runtime's programmed-deadline accounting.
- Advance an overdue periodic deadline in constant time with checked or wider
  arithmetic; never loop once per missed period in hard IRQ context. Normalize
  a zero interval and define saturation at the timestamp limit explicitly.
- Derive `TaskRuntime::timer_resolution_ns` from the platform counter frequency
  and round one hardware tick up to nanoseconds. Hard-coding 1 ns on AArch64,
  RISC-V, or LoongArch can convert `now + 1 ns` back to the current tick and
  create early or repeated immediate timer interrupts.
- In ArceOS, `ax_hal::irq::handle_irq` creates the return safe point only after
  controller EOI and after `irq-framework` clears its hard-IRQ marker. The
  outer preemption guard retains exactly one depth while hardware IRQs remain
  masked; `RuntimeSchedulerEntry::IrqReturn` atomically transfers that depth to
  the scheduler. Never create an enabled-IRQ window between the final
  preemption decrement and scheduler entry, and never schedule while the hard
  IRQ marker is live.
- Treat the CPU-local scheduler guard as a typed context-switch baton with
  `Active -> Transferred -> Finished` ownership. A resumed context consumes it
  when its suspended scheduler guard returns; a fresh kernel or idle
  context must call the runtime's initial-switch completion hook as its first
  operation, after completing scheduler switch tail and before touching TLS,
  taking context-aware locks, polling futures, or enabling interrupts.
- OS switch hooks may publish an address-stable scope/current identity, but
  must not retain a `Mutex`/`RwLock`, IRQ, or preemption guard across the naked
  context switch. Protect each scope-local access with its own bounded lease;
  writers publish intent before waiting so new readers cannot starve them.
  A read token that already owns the shared gate must expose a dedicated item
  capability rather than dereference to the unlocked `Scope` API: recursively
  acquiring a shared count can self-deadlock after a writer publishes upgrade
  intent.
  Move replaced `Arc` values out through the writer token and drop them only
  after IRQ/preemption guards are gone, and clone remote scope-owned handles
  before taking sleepable table or namespace locks.
- Keep the previous thread's `on_cpu` publication set until the architecture has
  physically left its stack. Clear it from switch tail in the newly active
  context; only then publish deferred migration or exit work. Switch tail must
  never run an OS exit hook or reap resources itself. A permanent ordinary-task
  service is the single bounded consumer for Deadline callbacks, exit hooks,
  thread reaping, and deferred resource reclamation. External handle/wake leases,
  `on_cpu`, timers, callback claims, and migration/policy inbox-delivery leases
  must all be clear before context/stack/extension destruction; a late delivery
  for an Exited thread is an idempotent no-op that only releases its retained
  payload.
- Make the address-space handoff explicit even when the next scheduler record
  carries the `NONE` sentinel. Capture the per-CPU kernel page-table root before
  publishing the CPU on x86_64 and RISC-V, where kernel and user execution share
  one root register; translate `NONE` back to that saved root. On AArch64 and
  LoongArch, where the kernel root is separate, translate `NONE` to a zero user
  root. `TaskRuntime::install_address_space` is the sole owner of CR3, TTBR,
  SATP, or PGDL installation. Resolve and install the root with local IRQs
  disabled after the previous switch-out hook and before the next switch-in
  hook; `TaskContext` must not cache, compare, or restore it.
- Validate every recoverable exit prerequisite before publishing join/exit
  completion. Use `prepare_current_exit() -> ExitPermit` followed by completion
  publication and the non-returning `commit_current_exit(permit)` so an ordinary
  scheduler error cannot leave an externally completed thread still running.
- Hold one `PreemptGuard` across the complete vCPU current-CPU scope: publish
  `CURRENT_VCPU`, bind/load host state, enter the guest, restore host anchors,
  unbind, and clear the per-CPU pointer. Backends should receive a borrowed
  pinned context; defer any blocking VM-exit handling until after the guard is
  released. VMX refreshes HOST_FS_BASE and HOST_GS_BASE on each bind; SVM
  restores host FS/GS through the host VMLOAD before returning to Rust;
  AArch64 restores host TPIDR_EL0; RISC-V restores host `sscratch`, canonical
  `gp`, and `tp`; and LoongArch restores host `r21`/KS3 and `tp` before calling
  Rust.
- Bound vCPU-exit handling may only copy fixed-size exit data or consume
  architecture state that must be captured before unbind. Hypercalls, MMIO,
  port I/O, system-register device callbacks, nested-page repair, CPU-up, and
  guest IPI fan-out run after backend unbind, `CURRENT_VCPU` removal, and
  preemption restoration. Deferred work must carry explicit VM/vCPU targets;
  it may recover the owning vCPU task from its thread extension in normal task
  context, but it must never republish a CPU-local current-vCPU header or use
  that task fallback from hard IRQ. Every post-bind error joins the same
  mandatory unbind path.
- Require a borrowed `BoundVcpu` capability for every live backend interrupt
  injection and pending-publication drain; ordinary `AxVCpu` references must
  not expose those operations. Device work performed after unbind publishes a
  typed pending interrupt, including edge/level metadata, through the existing
  `VMRef` runtime inbox. The next bound owner drains that inbox before guest
  entry. Publication succeeds before the scheduler kick, so a transient IPI
  failure may delay delivery but must not make callers retry an already
  committed edge. Backend injection failures return through the common unbind
  path instead of being logged and discarded.
- On RISC-V, represent a guest exit as `VmArchVcpuOps::Exit<'cpu>` and keep the
  host IRQ-save token private in that RAII value. The bound architecture
  handler must capture physical exit state before dropping the exit; its drop
  restores SIE while the adapter's outer `IrqGuard` is still active. Before the
  first guest run, validate the complete passthrough source set and target CPU,
  atomically lease the batch at PLIC priority zero, publish an immutable route
  descriptor containing owner, target CPU, and canonical source set, and only
  then activate every endpoint once. A conflicting monitor-wide owner or route
  descriptor must fail before platform preparation can mask or lease anything;
  after a successful atomic lease, publication and activation are fatal
  invariants rather than recoverable partial states. Never probe an enabled
  PLIC source by transiently writing a nonzero priority during validation.
- A RISC-V physical PLIC forward masks and completes the host claim before
  publishing only its canonical source/generation token. The hard-IRQ path may
  touch only the immutable source endpoint, preallocated lock-free ingress, and
  a stable direct wake handle: no allocation/free, logging, controller/domain
  lookup, driver lock, guest MMIO, or arbitrary callback. Busy or malformed
  leased state is quarantined and consumed rather than falling through to a
  host handler.
- Fix one vPLIC context as the platform owner. Only that owner may drain the
  VM-global forwarded ingress and guest-completion bitmaps, in batches of at
  most 64; nonowner vCPUs may synchronize only their own context line. Preserve
  the source generation until physical unmask, restore every unprocessed item
  after a decode/unmask failure without short-circuiting, and rearm the owner
  doorbell when a bounded batch leaves work. Guest claim/complete, priority,
  enable, and threshold MMIO updates publish all changed context lines and wake
  the completion owner after the MMIO operation returns to task context. Drain
  completion requests in task context, then unmask completed sources,
  synchronize HVIP, and enter the guest under one outer `IrqGuard` so no edge
  can be lost between unmask and entry.
- Before WFI, publish the CPU's polling/idle state, execute the architecture
  barrier, then recheck the owner runqueue, remote inbox and `need_resched` so a
  wake cannot be lost between the final check and sleep.
- A scheduler IPI is a typed CPU-local `IrqId`; keep its coalescing epoch in the
  OS-owned `CpuLocal` path and do not introduce an `ax-task` dependency into
  `ax-hal`, `ax-plat`, `somehal`, or `ax-percpu`.

## Validation Ladder

Run the smallest useful check first, then climb:

```bash
cargo test -p axbuild --lib
cargo xtask arceos test qemu --arch <arch>
cargo xtask starry test qemu --arch <arch>
cargo xtask axvisor test qemu --list --arch <arch>
cargo xtask axvisor test qemu --arch <arch> --test-group normal --test-case smoke
```

For LoongArch Axvisor LVZ validation, use the repository LVZ container and build `xtask` inside the container so embedded Cargo paths match the mounted workspace:

```bash
docker run --rm -v "$PWD:/workspace" -w /workspace \
  ghcr.io/rcore-os/tgoskits-container-axvisor-lvz:latest \
  bash -lc 'cargo xtask axvisor test qemu --arch loongarch64 --test-group normal --test-case smoke'
```

When Rust logic changes, also run the relevant targeted clippy command, usually:

```bash
cargo fmt
cargo xtask clippy --package axbuild
cargo xtask clippy --package someboot
```

Adjust the package list to match the crates touched.

## Debugging Workflow

1. Locate the last reliable print or machine state transition: UEFI entry, memory map, `ExitBootServices`, relocation, MMU enable, trap vector install, secondary release, first kernel print.
2. Add temporary byte-sized serial or MMIO markers around the transition. Remove them after finding the cause.
3. Use QEMU debug flags such as `-d int,cpu_reset,guest_errors` and `-S -s` when xtask exposes or can be patched to pass them.
4. Inspect symbols and generated images with `llvm-objdump`, `readelf`, and map files. Confirm runtime addresses, not only link addresses.
5. Compare with local Linux architecture code for ordering of MMU, trap, SMP, and cache/TLB barriers when uncertain. First search for a local Linux source tree, then inspect the matching `arch/<linux-arch>` directory; do not assume a fixed path.
6. On one-shot timer platforms, verify the IRQ handler acknowledges the current timer interrupt before dispatching into code that reprograms the next event. In particular, LoongArch timer handlers must not clear `TICLR` after `_handle_irq()` / `dispatch_irq()`, because the timer tick path may already have armed a near-deadline event and a late acknowledge can clear the freshly-pending interrupt, leaving timer-based sleeps stuck.
7. On RISC-V PLIC platforms, take ownership of every supervisor context before enabling `sie.SEXT`: clear inherited source-enable bits from firmware/bootloader state, initialize thresholds, and keep a software "source enabled" state instead of inferring enablement from non-zero priority. IRQ framework setup may set affinity while an action is still disabled; affinity changes must not enable a source until the framework explicitly enables the line.
8. Turn the root cause into a regression test or a focused QEMU case when practical.

## Common Failure Signals

- Hangs after `Exiting UEFI boot services...`: suspect stale memory map key, no post-exit console, wrong handoff address, MMU switch, or exception before trap vectors are valid.
- Fetch/load/store fault at high-half address: suspect kernel high map, direct map, DMW/window config, relocation offset, or wrong symbol address basis.
- TLB refill recursion or silent reset: suspect TLB refill entry physical address, trap vector mapping, stack mapping, or missing TLB flush.
- Secondary CPU never prints: suspect firmware CPU ID mapping, boot args cache visibility, secondary stack, per-CPU base, page table root, or per-secondary trap setup.
- Starry boots but interactive/system tests fail: suspect rootfs staging, input/console features, CPR/tty sizing assumptions, or success regex mismatches.
- Virtio block/rootfs missing: suspect PCI command enable, MMIO mapping, DMA address translation, virtio transport selection, or rootfs patch path.
- Axvisor only fails in LVZ container: verify container QEMU path, OVMF path, target toolchain, KVM/LVZ flags, and whether xtask was built inside the mounted workspace.

## Completion Criteria

- The change is validated at the smallest affected layer and at least one end-to-end QEMU path for the target OS.
- Temporary debug markers, QEMU one-offs, and local-only paths are removed or documented as intentional.
- `qemu-<arch>.toml`, `build-*.toml`, and OS configs only advertise architectures that were actually validated.
- New target/container/firmware requirements are documented in the relevant skill, test-suit guide, or docs page.
- If the task changes architecture boot logic, someboot startup order, UEFI handoff, SMP bring-up, dynamic platform contracts, target JSON assumptions, or the recommended debugging flow, update this skill or `references/boot-debugging.md` in the same change.
