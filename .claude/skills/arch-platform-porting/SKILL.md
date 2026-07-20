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
- **RISC-V per-CPU register contract**: `ax-percpu` reserves `x3`/`gp` as the per-CPU base, so every RISC-V kernel target spec must pass `--no-relax` to the linker. Do not enable global-pointer relaxation or define `__global_pointer$` unless the per-CPU register design changes at the same time.
- **Build system**: wire arch/target mapping in `scripts/axbuild`, dynamic platform defaults, feature propagation, kernel format conversion, UEFI/to-bin behavior, rootfs handling, and per-OS test discovery.
- **QEMU and firmware**: verify QEMU binary, machine type, CPU, SMP count, pflash/OVMF files, serial console, disk/rootfs device, `-snapshot`, debug flags, timeout, and success/fail regexes.
  QEMU `uefi`, `to_bin`, acceleration, CPU feature, and device choices are part of each
  `qemu-*.toml` contract; axbuild must not infer or overwrite them from the target architecture
  or host `/dev/kvm` availability.
  Axvisor x86_64 selects the VMX or SVM backend at runtime from CPUID; the generic QEMU board
  and all Axvisor build configs remain backend-neutral. CI must retain separate Intel/VMX and
  AMD/SVM QEMU cases because their host CPU exposure differs, but neither case may select a
  Cargo `vmx` or `svm` feature. Both cases must use the same backend-neutral guest baseline so
  their result isolates the runtime CPUID-selected virtualization path.
- **someboot arch layer**: implement or audit entry, relocation, BSS clearing, stack setup, memory map parsing, paging, trap vectors, timer, IRQ, power, SMP, and address translation.
- **CPU runtime**: update `components/axcpu/src/<arch>` for trap entry, context switch, user/kernel context, syscall return path, FP/SIMD state, and per-CPU assumptions.
- **Platform bridge**: update `platforms/axplat-dyn`, `platforms/somehal`, platform config, memory regions, IRQ routing, timer source, power operations, and CPU boot operations.
- **Runtime platform identity**: dynamic platform names should be discovered in `someboot`/`somehal` from firmware data, then exposed through `axplat-dyn` and `ax_plat::platform::platform_name()`. Keep `ax-hal` as a forwarding layer for platform identity, and keep static platforms returning `config::PLATFORM`.
- **Runtime IRQ ownership**: ArceOS runtime IRQ traps are owned by `ax-cpu` and dispatched through `ax_hal::irq::handle_irq(raw_vector)`, which immediately wraps the CPU trap entry as `TrapVector`. `somehal` must stay OS-free and expose controller transactions through `somehal::irq::begin_irq(raw_vector) -> ActiveIrq`; `ActiveIrq::id()` returns the resolved `IrqId`, and `ActiveIrq` is held while `axplat-dyn` dispatches the IRQ and its `Drop` performs the architecture-specific EOI/complete. Do not reintroduce `_someboot_handle_irq` or `#[somehal::irq_handler]` as runtime dispatch glue.
- **Runtime IRQ initialization order**: dynamic platforms initialize boot IRQ state through `ax_hal::irq::init_boot_irqs(cpu_id)` before registering runtime IRQ handlers or probing normal devices. `rdrive::ProbeLevel` remains the coarse lifecycle boundary, and `ProbePriority` is the ordering source inside `PreKernel`: clocks first, then interrupt controllers, timer sources, MSI parent controllers, and only later normal early devices. For FDT, same level/priority matches must keep device-tree order; interrupt-controller nodes additionally follow parent-before-child ordering similar to Linux `of_irq_init()`, with sibling controllers preserving DT order. Do not add arch-specific ad hoc probe calls in `axruntime` when a priority barrier can express the same dependency.
- **Runtime IPI identity**: dynamic platforms expose the runtime IPI IRQ as a typed `IrqId` through `somehal::irq::ipi_irq()`, `axplat-dyn`, and `ax_hal::irq::ipi_irq()`. Do not route dynamic runtime IPI registration through `ax-config`; on RISC-V the IRQ is the flagged supervisor software interrupt cause in the CPU-local domain, not bare PLIC source `1`.
- **Runtime CPU limits**: treat generated `CPU_CAPACITY`/`SMP` as a build-time capacity for const generics, per-CPU arrays, and linker/percpu layout only. Actual online/usable CPU count must flow through `ax_hal::cpu_num()`, which caps the platform-discovered count by capacity.
- **IRQ namespace rules**: keep CPU trap vectors, platform `IrqId { domain, hwirq }`, firmware sources (`IrqSource::AcpiGsi`, `IrqSource::AcpiGsiRoute`, explicit `IrqSource::ControllerLine`, and driver binding metadata such as `BindingIrqSource::FdtInterrupt`), controller-local hardware lines (`HwIrq`), and guest GSI/vector values in separate namespaces. New runtime IRQ registrations must use `IrqId`, not `usize`; legacy `IrqNumber(raw)` is only for static or still-unmigrated platform boundaries and must live in OS/HAL-facing layers such as `ax-plat`, `ax-hal`, or `axklib`, not `irq-framework` or `somehal`. `irq-framework` owns generic registry, affinity, execution, and boxed callback dispatch semantics; platform rebase work must preserve `BoxedIrqHandler`, `IrqExecution`, and `IrqRequest::new_boxed` while adapting the surrounding platform code to `IrqId`. `LEGACY_IRQ_DOMAIN` and `CPU_LOCAL_IRQ_DOMAIN` remain fixed compatibility domains, while dynamic `somehal` external controller domains such as GIC, PLIC, IOAPIC, EIOINTC, and PCH-PIC are allocated at controller probe time and must be reached through `alloc_irq_domain`, `domain_by_kind`, `domain_by_owner`, or `domain_is_kind`, not by constructing fixed numeric controller domains in dynamic-platform code. Do not derive a host IRQ with arithmetic such as `0x20 + gsi`, `PCI_INTX_VECTOR_BASE + gsi`, or by subtracting a trap-vector base in Axvisor/device code. Resolve firmware/device descriptions with `ax_hal::irq::resolve_irq_source(...)` / platform resolver and register the returned `IrqId`. When ACPI supplies trigger/polarity/controller metadata, carry it as `IrqSource::AcpiGsiRoute` instead of flattening it to a bare `AcpiGsi`, because PCI INTx routes may use a low GSI with non-ISA level/low semantics. Likewise, FDT device bindings should carry the raw interrupt specifier plus its controller owner in `BindingIrqSource::FdtInterrupt` until the OS/platform layer can resolve that owner to a controller domain and configure it; do not expose parentless FDT cells from `irq-framework` or configure a controller in generic driver probe merely to obtain a legacy number. `rdif_intc` controllers must expose fallible `translate_fdt` / `translate_acpi` methods that return controller-local hardware line and trigger metadata; the registering platform allocates or looks up a domain owner entry for the concrete `rdrive::DeviceId`, passes that domain to `rdif_intc::Intc::new(domain, driver)`, and the wrapper combines that domain with the local `HwIrq` before `configure` / `configure_acpi` programs trigger, polarity, vector, or mask state. Platform `irq_set_enable` and `irq_set_affinity` paths must route by the incoming domain's registered owner/kind and return an error on missing controllers, lock failures, unsupported affinity, or backend/type mismatches instead of silently no-oping. Empty, malformed, out-of-range, or unsupported firmware specifiers must return `IrqError` instead of IRQ 0, a base vector, or a guessed legacy number. If an FDT PCI host bridge preconfigures a controller-level legacy INTx route, store that route as a native `BindingIrq` source (plus any temporary raw compatibility value) and let child endpoints reuse it before falling back to PCI `interrupt-map` parsing.
- **Domain expectations**: x86 LAPIC timer and IOAPIC are distinct domains, so trap vector `0x20` is not `AcpiGsi(0)`. On aarch64, GIC INTID is the `HwIrq` within the GIC domain. On riscv64, PLIC source is the `HwIrq` within the PLIC domain. On loongarch64, EIOINTC and PCH-PIC must remain separate domains. A platform that cannot resolve an `IrqSource` must return `IrqError::Unsupported` instead of guessing a numeric IRQ.
- **x86 QEMU IRQ contract**: the dynamic x86 path targets modern QEMU `q35` with ACPI/MADT, Local APIC or x2APIC, IOAPIC, and PCI INTx routing. Do not add 8259/PIC fallback, i440fx-specific IRQ assumptions, non-ACPI IRQ probing, raw GSI enable bypasses, or vector arithmetic outside the IOAPIC controller. LAPIC/x2APIC owns timer, IPI, EOI, and spurious handling; `X86IoApicIntc` owns external GSI route state, vector conflict checks, trigger/polarity, mask, and affinity updates through `rdif_intc::Intc`. x2APIC paths must preserve full `u32` APIC IDs for CPU-local operations, while xAPIC and IOAPIC physical destinations must reject APIC IDs that cannot be encoded without truncation.
- **LoongArch QEMU IRQ contract**: the dynamic LoongArch path targets QEMU `virt`/LS7A-style firmware routing through CPU-local timer/IPI lines, EIOINTC, and PCH-PIC. `somehal::begin_irq(raw)` receives the CPU interrupt line from `ESTAT.IS`, not an ACPI GSI or PCI vector; only the timer line, IPI line, and EIOINTC cascade line may enter runtime dispatch. EIOINTC owns claim/complete of external vectors, while PCH-PIC owns PCH input state, ACPI trigger/polarity configuration, mask state, and route memory through its `rdif_intc::Intc` lock. Do not infer PCH-PIC input by subtracting `PCI_INTX_VECTOR_BASE`, do not treat ACPI `route.vector = PCI_INTX_VECTOR_BASE + gsi` as the EIOINTC hardware vector, and do not dispatch unknown CPU-local interrupt lines as PCH-PIC IRQs.
- **RISC-V QEMU IRQ contract**: the dynamic RISC-V path targets QEMU `virt` firmware routing through CPU-local supervisor timer/software/external interrupt causes and one PLIC domain. `somehal::begin_irq(raw)` receives `scause.bits()`, not a PLIC source number; only S-timer, S-soft, and S-ext are runtime CPU-local causes. PLIC source IDs are controller-local `HwIrq`s and may only be produced by FDT translation or by claiming the PLIC after an S-ext trap. Do not dispatch a bare source number as a trap, do not treat PLIC source 0 as valid, and route PLIC enable through the registered `rdif_intc::Intc` controller instead of bypassing the rdrive lock.
- **Runtime console selection**: Dynamic platforms expose the firmware-selected hardware console through `somehal::console_device_id()` and `ax_hal::console::device_id()`. The value is `Result<rdrive::DeviceId, ConsoleDeviceIdError>` derived from bootargs `console=`, ACPI SPCR, or FDT `stdout-path`; static platforms return `Err(NotSpecified)`. OS code such as Starry should match `Ok(id)` against probed serial devices, use `ttyS0` as the Linux-style hardware-console fallback only for `Err(NotSpecified)`, and leave `/dev/console` unbound (`ENODEV`) for non-hardware console selections, unmatched selected hardware devices, or when no serial console TTY exists. Do not reparse FDT or bootargs in the tty layer.
- **Runtime console ownership**: once Starry or another OS runtime binds the firmware-selected UART to an interrupt-driven tty/serial driver, call `ax_hal::console::claim_runtime_output()` and stop the low-level boot/platform console path from writing the same UART registers directly. The hardware console must have one runtime register owner; otherwise kernel log output and tty output can interleave at the UART register level and corrupt test markers or user input/output.
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
3. Prepare one boot argument block per secondary CPU with stack, page table, kernel entry, per-CPU base, and logical ID.
4. Flush boot arguments and page tables before `cpu_on`.
5. In the secondary path, initialize arch address windows, stack, per-CPU register, page table state, trap vectors, timer, and interrupt state before entering generic secondary code.
6. Before the OS per-CPU register is initialized on a secondary CPU, use cached controller fast paths for interrupt and timer setup through `somehal::irq::init_secondary_boot_irqs(cpu_id)`; do not take `rdrive`, IRQ-domain, or generic route locks from that window.
7. Debug secondary failure with physical-address markers first; serial logging may not work until the secondary has its own mapping and trap state.

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
