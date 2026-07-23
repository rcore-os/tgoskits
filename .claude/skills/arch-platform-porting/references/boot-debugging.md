# Boot Debugging Reference

This reference captures project-specific lessons from enabling LoongArch dynamic UEFI platform boot, someboot SMP, StarryOS tests, and Axvisor LVZ smoke testing.

## Layer Map

| Layer | Typical files | What must agree |
| --- | --- | --- |
| Target spec | `scripts/targets/**/<triple>.json` | ABI, soft-float, relocation model, linker, panic, std/musl support |
| Build orchestration | `scripts/axbuild/src/{build.rs,context,test/qemu.rs,*}` | arch to target mapping, features, UEFI mode, QEMU command, rootfs image |
| Test data | `test-suit/{arceos,starryos,axvisor}/**` | runtime TOML, build TOML, regexes, SMP count, firmware mode |
| Bootloader | `platforms/someboot/src/**` | entry ABI, relocation, memory map, paging, trap, SMP, power |
| CPU runtime | `components/axcpu/src/<arch>/**` | trap frame layout, context switch, FP/SIMD, user return |
| Dynamic platform | `platforms/{axplat-dyn,somehal}/**` | runtime memory/IRQ/timer/power facts from firmware |
| Drivers | `drivers/**`, `patches/virtio-drivers/**` | MMIO/iomap, DMA, PCI command bits, virtio transport |

When a boot failure appears in a high layer, still audit lower-layer contracts. For example, a Starry rootfs failure can be caused by PCI command bits, and an Axvisor hang can be caused by a someboot post-UEFI handoff.

## CPU-local Register Ownership

`cpu-local` is the single owner of host CPU-area, current-thread, and kernel-TLS register
semantics. `ax-percpu` supplies the typed template/layout/area implementation but must not choose
an architecture register independently. The two image modes are mutually exclusive at a final
image boundary:

| Architecture | CPU area | Linux-current image | Unikernel-TLS image |
| --- | --- | --- | --- |
| x86_64 | GS base | current header in `CpuRuntimeAnchor` | FS base is task TLS |
| AArch64 | TPIDR_EL1 at EL1, TPIDR_EL2 at EL2 | SP_EL0 is current | TPIDR_EL0 is task TLS |
| RISC-V | recover from current header, or sscratch | tp is current and sscratch is zero in kernel Rust | tp is task TLS and sscratch is CPU base |
| LoongArch | r21, mirrored in KS3 | tp is current | tp is task TLS |

The final ELF owns exactly one `.percpu.template`, one `.percpu.init` descriptor table, and one
`.percpu.align` table. someboot or another platform allocates the runtime areas dynamically from
that geometry, initializes every typed object at its final address, freezes the layout, and only
then binds a CPU. There is no linked runtime alias and the template size must not depend on SMP.
Linker boundaries use only `__PERCPU_*` and `__CPU_LOCAL_*`; x86 trap entry consumes the relative
`__CPU_LOCAL_TSS_OFFSET`.

The exact initialized `CpuAreaRef` address is the area identity. One final image has no CPU-local
ABI version, layout generation, cookie, or provider-trait FFI. A `CpuPin<'scope>` validates the
live CPU base, prefix self pointer/index, and current header and cannot escape its guard. Atomic
scalars require migration exclusion; shared `T: Sync` values require the same pin plus their own
synchronization; mutable local objects require `ExclusiveCpu` after IRQ/re-entry and conflicting
remote access are excluded. CPU-area construction is permitted only while that CPU is offline and
the raw destination is exclusively owned.

Context-switch publication follows one ordering: validate the outgoing binding, bind the next
stable task header, prepare every fallible state transition, commit the architecture register,
perform the naked switch, and unbind the previous header in the incoming tail. The interrupt-off
`CpuPin` spans that sequence. An uncommitted prepared token rolls the next binding back, while the
previous binding epoch rejects a stale incoming tail after task rebinding; that epoch is a runtime
concurrency guard, not an ABI version. vCPU exits must restore the host register contract before returning
to host Rust; LoongArch KS4/KS5 remain vCPU scratch and AArch64 must restore host TPIDR_EL0 before
calling Rust exception handlers.

For boot debugging, verify the typed per-CPU layout is finalized and frozen before CPU binding.
Check both the architectural register and its defined mirror (RISC-V sscratch or LoongArch KS3)
on secondaries. A separate current-task per-CPU variable can mask a stale register during normal
execution and then fail only on traps or vCPU exits, so it is not a valid fallback.

## Final Image Runtime Modes

- Starry uses its original bare target as a `no_std`/`no_main` PIE with
  `build-std=core,alloc`; SMP remains a compile-time capability, while the runtime CPU limit is
  configured separately. Its ELF must be `ET_DYN` without `PT_TLS`, `.tdata`, or `.tbss`.
- Axvisor remains a std/musl PIE and explicitly selects the complete TLS chain down through
  axruntime, axhal, `cpu-local`, axvm, axplat-dyn, somehal, and someboot. AxVM snapshots the host
  kernel-TLS value around each guest transition in addition to validating the exact CPU area.
- ArceOS retains TLS by default. A userspace build owns the same architecture register for
  Linux-current semantics, so `uspace + tls` is a configuration error.
- someboot renders TLS and no-TLS linker layouts separately. For relocatable direct images,
  audit the final ELF at multiple load biases and accept only the architecture's supported
  relative relocation types.

## AArch64 Axvisor EL2 Checks

- The Axvisor `hv` feature chain must select `ax-cpu/arm-el2` only for AArch64. Keep the chain
  `ax-hal/hv` -> `axplat-dyn/hv` -> `somehal/hv`; `somehal`'s AArch64-only optional `ax-cpu`
  dependency owns the `arm-el2` edge. A successful AArch64 compile does not prove that the EL2
  register implementation was selected, while an unconditional edge would incorrectly enable it
  for other architectures.
- If an EL2 image compiles the EL1 page-table path, `ax-mm` can appear to initialize normally while
  the new root is written to `TTBR1_EL1`. The active `TTBR0_EL2` then remains the someboot table,
  so the first access to a dynamically mapped device can fault or look like a hang. On PhytiumPi,
  the characteristic stop is the first GIC distributor read immediately after the rdrive FDT
  initialization message.
- Confirm the runtime reports `EL: 2`, inspect the resolved `ax-cpu` feature set for `arm-el2`, and
  verify that a post-`ioremap` MMIO access succeeds before instrumenting the device driver itself.
- Axvisor QEMU and board test cases own their CPU-count contract. Test requests must discard an
  interactive snapshot's `smp` value; otherwise a stale `tmp/axbuild/.axvisor.toml` can silently
  shrink the host. A Phytium guest assigned to logical CPU 2 will then fall back to CPU 0 and may
  stop at its first virtual timer interrupt even though the vCPU world switch is correct.

## Dynamic UEFI Platform Notes

- Dynamic platform means the platform facts come from firmware/runtime discovery through `someboot`, `somehal`, and `axplat-dyn`. It does not remove the need for arch-specific page table, trap, timer, IRQ, and power code.
- Match the x86_64 dynamic UEFI path first: firmware disk layout, `to_bin` behavior, pflash/OVMF handling, and handoff expectations.
- Keep dynamic platform features aligned across `ax-std`, `ax-hal`, `ax-driver`, `axvm`, and the OS package. A partial `plat-dyn` feature set often compiles but fails after device or memory init.
- For std/musl targets, derive the initial JSON from a known Rust target where possible, then minimally adjust ABI, linker, relocation model, and soft-float. A `none-softfloat` target passing does not prove musl/std ABI correctness.
- Prefer runtime memory map data over board constants. Any early helper such as `phys_to_virt` must be valid for the phase where it is called.

## someboot Startup Checklist

Use this order when auditing an early boot port:

1. Entry preserves firmware arguments and records them before BSS or relocation can destroy them.
2. Early serial output works before `ExitBootServices`.
3. Firmware memory map is captured, classified, and converted into the kernel memory model.
   For FDT input, enumerate every tuple in each `/reserved-memory` `reg`
   property and fail explicitly if a fixed-capacity collection cannot retain
   every usable RAM range; silently dropping later ranges can expose reserved
   pages or leave RAM unmapped. When one early bump arena is required, select
   the largest nonempty valid free range instead of depending on firmware
   descriptor order. Do not impose an arbitrary size threshold: checked bump
   allocation must report when the selected range cannot hold the actual boot
   objects.
4. Kernel image physical range, load offset, and high-half range are known before address translation helpers are used.
5. Page tables or arch direct-map windows cover the currently executing code, boot stack, page tables, kernel high map, MMIO, and boot data.
6. Trap vectors are installed using the address form required by the architecture at that moment.
7. MMU enable is followed by the required barrier, TLB flush, and an address-basis-safe jump.
8. Post-MMU console and panic paths are usable.
9. The single ELF CPU-local template and descriptor tables are resolved after relocation.
10. Runtime CPU areas and secondary boot stacks are dynamically allocated; every typed area is
    initialized once, frozen, and bound through the architecture CPU-local register contract.
11. Secondary CPU release happens only after boot arguments and page tables are visible to other CPUs.

## RISC-V FDT SMP Notes

- Enumerate only CPU nodes that firmware marks available. A missing `status` property is usable, `status = "okay"`/`"ok"` is usable, and `status = "disabled"` must be skipped.
- Keep FDT `reg` hart IDs as firmware CPU IDs and map them onto dense logical CPU IDs separately. On VisionFive2, `cpu@0` is a disabled S7 management hart while the usable U74 cores are `cpu@1` through `cpu@4`; full-core boot should therefore start from hart 1 and bring up harts 2-4, not fall back to single-core mode.
- If a RISC-V board traps when secondaries are released, dump `/cpus` from the boot FDT before changing `max_cpu_num`; disabled or non-OS CPU nodes are a common cause of `cpu_on` targeting the wrong hart.

## LoongArch Lessons

- For U-Boot FIT boot, keep the producer and handoff contracts aligned: use the canonical FIT architecture name `loongarch`, ensure U-Boot passes the DTB at a DTSpec-compliant 8-byte-aligned address, and hand a FIT-provided FDT to someboot through the UHI convention (`a0 = -2`, `a1 = fdt`). Vendor `CONFIG_LOONGSON_BOOT_FIXUP` paths that inspect `legacy_hdr_os` must not run for FIT images.
- TLB refill entry and general exception entry use different registers and may require different address forms. Do not reuse a high-half virtual symbol where a physical TLB refill vector is required.
- Relocated symbols must be resolved relative to the running image. In the LoongArch SMP path, the secondary exception vector had to use a runtime symbol helper such as `sym_running_addr!(__exception_vectors)`, while the TLB refill entry needed the corresponding physical address.
- A secondary CPU can fault before it has a working serial path. Put markers before and after DMW setup, stack switch, page table register setup, trap-vector setup, and jump to the common secondary entry.
- Initialize trap vectors on every CPU, not only the boot CPU.
- Flush or barrier boot arguments before `cpu_on`; otherwise secondaries can observe stale stack, page table, or per-CPU data.
- Keep logical CPU ID mapping separate from firmware CPU IDs. LoongArch CPU IDs in firmware data are not guaranteed to be dense array indices.
- Compare ordering with local Linux architecture code when uncertain. For LoongArch, useful topics include DMW setup, CSR write ordering, TLB refill vector, exception entry, SMP boot argument handoff, and cache/TLB barriers.

## Finding Local Linux Source

When Linux behavior is useful as an architecture reference, look for a local kernel tree before relying on memory or online search:

```bash
find "$PWD" "$PWD/.." "$HOME" /home -maxdepth 4 -type f -name Makefile \
  -path '*/linux*/Makefile' 2>/dev/null
```

Verify a candidate by checking for a top-level `Makefile`, `Kconfig`, and the target architecture directory. Common directory names differ from Rust target names:

| Project arch | Linux arch directory |
| --- | --- |
| `loongarch64` | `arch/loongarch` |
| `x86_64` | `arch/x86` |
| `aarch64` | `arch/arm64` |
| `riscv64` | `arch/riscv` |

Search the local tree with `rg` before opening large files. Good first patterns include `setup_arch`, `start_kernel`, `smp_prepare_cpus`, `secondary_start`, `cpu_up`, `set_exception`, `tlb`, `fixmap`, and architecture-specific CSR/register names.

## Axvisor LVZ Container Notes

Use the LVZ container for LoongArch Axvisor validation because host QEMU may not include the needed LVZ support:

```bash
docker run --rm -v "$PWD:/workspace" -w /workspace \
  ghcr.io/rcore-os/tgoskits-container-axvisor-lvz:latest \
  bash -lc 'cargo xtask axvisor test qemu --arch loongarch64 --test-group normal --test-case smoke'
```

Important details:

- Build and run `cargo xtask` inside the container. A host-built `target/debug/tg-xtask` can embed a host `CARGO_MANIFEST_DIR` path that does not exist inside `/workspace`.
- Check `/opt/qemu-lvz/bin/qemu-system-loongarch64`, OVMF files under `/tmp/ostool/ovmf/loongarch64`, and the musl toolchain before assuming the kernel is at fault.
- If output reaches `Exiting UEFI boot services...` and stops before the next someboot print, instrument immediately before and after `ExitBootServices`, memory map handoff, first post-exit console call, and MMU/trap setup.
- Container success still needs host-independent documentation if the CI or developer flow depends on that image.

## QEMU Debugging Patterns

- Add `-S -s` to stop at reset and attach GDB when the failure is before the first reliable print.
- Add `-d int,cpu_reset,guest_errors` to capture traps, resets, and invalid guest accesses.
- Use short serial markers for phase isolation. Example phases: `E` for UEFI entry, `M` for memory map, `X` before exit boot services, `x` after exit, `P` before paging, `p` after paging, `T` after trap vectors, `S` before secondary release.
- Remove markers before finalizing unless they become intentional diagnostics.
- If QEMU is launched by `ostool`, patch the local ostool or xtask wrapper temporarily rather than hand-assembling a different command line. The reproduced command must remain faithful to the failing path.

## Symptom Triage

| Symptom | First suspects |
| --- | --- |
| Stops at or after UEFI exit | memory map key, Boot Services call after exit, post-exit console, handoff address, trap before vectors |
| Immediate reset after MMU enable | wrong page table root, missing identity/current mapping, bad barrier/TLB flush, invalid jump target |
| High-half fetch fault | kernel high map, relocation offset, symbol address basis, direct-map window |
| TLB refill recursion | TLB refill vector address, stack mapping, refill handler mapping, CSR ordering |
| Secondary CPU silent | `cpu_on` argument, cache flush, stack, per-CPU base, trap setup, logical CPU ID mapping |
| ArceOS works but Starry fails | rootfs staging, std/musl ABI, console/input feature, tty assumptions, CPR sizing |
| Starry shell works but grouped tests fail | generated runner path, copied assets, success regex, `shell_init_cmd` versus `test_commands` |
| AArch64 Axvisor stops at first dynamic MMIO read | missing `ax-cpu/arm-el2`, inactive EL1 page-table root, stale `TTBR0_EL2` boot table |
| Phytium guest stops after `arch_timer` | inherited board-test SMP limit, vCPU CPU-mask fallback, virtual timer routing |
| Axvisor build works but QEMU hangs | firmware/OVMF path, LVZ QEMU, guest image/rootfs, dynamic platform memory map, post-UEFI transition |
| Virtio block missing | PCI command enable, virtio transport, MMIO map, DMA translation, rootfs disk args |

## Validation Recipe From This Bring-Up

These commands form a practical ladder for LoongArch dynamic platform work:

```bash
cargo test -p axbuild --lib
cargo xtask arceos test qemu --arch loongarch64
cargo xtask starry test qemu --arch loongarch64
docker run --rm -v "$PWD:/workspace" -w /workspace \
  ghcr.io/rcore-os/tgoskits-container-axvisor-lvz:latest \
  bash -lc 'cargo xtask axvisor test qemu --list --arch loongarch64'
docker run --rm -v "$PWD:/workspace" -w /workspace \
  ghcr.io/rcore-os/tgoskits-container-axvisor-lvz:latest \
  bash -lc 'cargo xtask axvisor test qemu --arch loongarch64 --test-group normal --test-case smoke'
```

If logic changed in the relevant crates, run targeted clippy after formatting:

```bash
cargo fmt
cargo xtask clippy --package axbuild
cargo xtask clippy --package someboot
cargo xtask clippy --package ax-cpu
cargo xtask clippy --package axplat-dyn
cargo xtask clippy --package ax-driver
```

Adjust the package set to the actual diff. Documentation-only skill updates do not require clippy.
