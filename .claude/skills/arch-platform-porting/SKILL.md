---
name: arch-platform-porting
description: Add, adapt, debug, or review architecture/platform support for ArceOS, StarryOS, Axvisor, someboot, dynamic UEFI platform boot, SMP startup, QEMU boot configs, target JSON files, axbuild arch mapping, axcpu trap/context code, axplat-dyn, somehal, and LoongArch/x86/aarch64/riscv platform bring-up issues.
---

# Arch Platform Porting

Use this skill when adding or fixing an architecture, switching QEMU cases to dynamic UEFI platform boot, enabling SMP in someboot, debugging early boot hangs, or validating ArceOS/StarryOS/Axvisor on a new arch/platform path.

For detailed pitfalls and debugging notes from the LoongArch dynamic UEFI/SMP bring-up, read `references/boot-debugging.md` when the task touches early boot, trap vectors, MMU, SMP, UEFI exit, or Axvisor LVZ QEMU.

Current Axvisor LoongArch QEMU tests intentionally use the static `ax-hal/loongarch64-qemu-virt` platform, non-UEFI QEMU boot, and an ELF kernel image without BIN conversion. Treat Axvisor LoongArch dynamic UEFI bring-up as a separate task unless the user explicitly asks to change that platform mode.

## First Pass

1. Identify the layer that is changing: target spec, axbuild, test-suit config, someboot, axcpu, axplat-dyn/somehal, device driver, or OS config.
2. Inspect the closest working architecture first. For dynamic UEFI paths, compare with x86_64 before inventing new behavior.
3. Trace the full boot contract from QEMU args to kernel entry. Do not assume a QEMU config change is enough if the firmware, target ABI, loader, and runtime platform disagree.
4. Prefer `cargo xtask` flows for ArceOS, StarryOS, and Axvisor. If a special QEMU/container setup needs raw commands, inspect the xtask path and match its arguments.
5. Keep temporary debug markers out of the final patch unless the user explicitly asks to retain them.

## Porting Checklist

- **Target and toolchain**: add or verify `scripts/targets` specs, target triple, panic strategy, relocation model, code model, ABI, soft-float setting, musl/std support, linker, objcopy, and `rust-src` availability.
- **Build system**: wire arch/target mapping in `scripts/axbuild`, dynamic platform defaults, feature propagation, kernel format conversion, UEFI/to-bin behavior, rootfs handling, and per-OS test discovery.
- **QEMU and firmware**: verify QEMU binary, machine type, CPU, SMP count, pflash/OVMF files, serial console, disk/rootfs device, `-snapshot`, debug flags, timeout, and success/fail regexes.
- **someboot arch layer**: implement or audit entry, relocation, BSS clearing, stack setup, memory map parsing, paging, trap vectors, timer, IRQ, power, SMP, and address translation.
- **CPU runtime**: update `components/axcpu/src/<arch>` for trap entry, context switch, user/kernel context, syscall return path, FP/SIMD state, and per-CPU assumptions.
- **Platform bridge**: update `platforms/axplat-dyn`, `platforms/somehal`, platform config, memory regions, IRQ routing, timer source, power operations, and CPU boot operations.
- **Runtime IRQ ownership**: ArceOS runtime IRQ traps are owned by `ax-cpu` and dispatched through `ax_hal::irq::handle_irq`. `somehal` must stay OS-free and expose controller transactions through `somehal::irq::begin_irq(raw) -> ActiveIrq`; `ActiveIrq` is held while `axplat-dyn` dispatches the IRQ and its `Drop` performs the architecture-specific EOI/complete. Do not reintroduce `_someboot_handle_irq` or `#[somehal::irq_handler]` as runtime dispatch glue.
- **Dynamic firmware devices**: for `rdrive` ACPI probes, real non-empty ACPI ID lists enumerate namespace `Device` nodes and expose `_CRS` memory, I/O port, and IRQ resources through `AcpiInfo`; empty ID lists or synthetic root IDs are reserved for root-table style callbacks.
- **Page tables and memory**: check PTE flags, huge page support, direct map, kernel high map, MMIO map, TLB/cache barriers, and early `phys_to_virt` behavior before MMU state is fully recorded.
- **Drivers and rootfs**: check PCI command bits, MMIO/iomap, DMA address width, virtio transport, block device visibility, rootfs patching, and console/input feature flags.
- **OS configs and test cases**: update ArceOS, StarryOS, and Axvisor configs only for validated architectures. Keep `qemu-<arch>.toml` runtime config separate from `build-*.toml`.

## someboot Must-Haves

- Preserve the firmware entry ABI. UEFI entry carries `image_handle` and `system_table`; direct boot paths use different arguments.
- Establish an early console before risky transitions, then ensure a post-UEFI/post-MMU console path exists without Boot Services.
- Capture the memory map and kernel image physical range before address translation helpers depend on them.
- Treat relocated symbols carefully. After relocation or high-half switch, use runtime-safe symbol address helpers instead of raw compile-time addresses.
- Clear BSS exactly once and after preserving any entry data that lives there.
- On LoongArch OVMF, capture the EFI FDT configuration table as well as ACPI RSDP for firmware-described devices, but do not rediscover RTC in someboot/somehal through those tables. The dynamic UEFI RTC path should first use the UEFI Runtime Service `GetTime`; LS7A RTC nodes such as `loongson,ls7a-rtc` and ACPI `LOON0001` belong to the `ax-driver` fallback path when firmware RTC is unavailable.
- Allocate and align boot stack, per-CPU areas, secondary stacks, boot arguments, and page tables before enabling SMP.
- Install trap vectors before enabling interrupts, timer interrupts, MMU faults, or secondary CPU execution.
- On x86 QEMU, do not trust CPUID timing leaves unless the reported TSC frequency is plausible; some virtual CPU combinations expose invalid zero or tiny values. Prefer a trusted hypervisor timing leaf, then CPUID timing data, then PIT-based TSC calibration before falling back to processor base frequency.
- On AArch64, keep the someboot `hv` feature scoped to the EL2 kernel path. For non-`hv` EL1 boot, choose the EL1 arch timer at runtime from the boot EL: use CNTP when EL2 is available and CNTV when EL2 is unavailable, and keep the FDT timer interrupt index consistent with the selected mode.
- Build page tables for identity/firmware access, direct map, kernel high map, MMIO, and per-CPU data as the arch requires.
- Flush TLB/cache and use architecture barriers around page table writes, boot argument writes, and secondary CPU release.
- After `ExitBootServices`, do not call UEFI Boot Services. Retry only through the correct memory-map-key sequence before exit.

## SMP Bring-Up Rules

1. Discover enabled CPUs from firmware data and keep firmware IDs separate from logical CPU IDs.
2. Bound-check CPU indices and avoid assuming hart/apic/mpidr/cpuid values are dense.
3. Prepare one boot argument block per secondary CPU with stack, page table, kernel entry, per-CPU base, and logical ID.
4. Flush boot arguments and page tables before `cpu_on`.
5. In the secondary path, initialize arch address windows, stack, per-CPU register, page table state, trap vectors, timer, and interrupt state before entering generic secondary code.
6. Debug secondary failure with physical-address markers first; serial logging may not work until the secondary has its own mapping and trap state.

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
7. Turn the root cause into a regression test or a focused QEMU case when practical.

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
