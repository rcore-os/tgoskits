# Boot Debugging Reference

This reference captures project-specific lessons from enabling LoongArch dynamic UEFI platform boot, someboot SMP, StarryOS tests, and Axvisor LVZ smoke testing.

## Layer Map

| Layer | Typical files | What must agree |
| --- | --- | --- |
| Target spec | `scripts/targets/**/<triple>.json` | ABI, soft-float, relocation model, linker, panic, std/musl support |
| Build orchestration | `scripts/axbuild/src/{build.rs,context,test/qemu.rs,*}` | arch to target mapping, features, UEFI mode, QEMU command, rootfs image |
| Test data | `test-suit/{arceos,starryos,axvisor}/**` | runtime TOML, build TOML, regexes, SMP count, firmware mode |
| Bootloader | `platforms/someboot/src/**` | entry ABI, relocation, memory map, paging, trap, SMP, power |
| CPU runtime | `components/axcpu/src/<arch>/**` | trap frame layout, context switch, FP/SIMD, user return, runtime stage-1 PTE format and TLB semantics |
| Dynamic platform | `platforms/{axplat-dyn,somehal}/**` | runtime memory/IRQ/timer/power facts from firmware |
| Drivers | `drivers/**`, `patches/virtio-drivers/**` | MMIO/iomap, DMA, PCI command bits, virtio transport |

When a boot failure appears in a high layer, still audit lower-layer contracts. For example, a Starry rootfs failure can be caused by PCI command bits, and an Axvisor hang can be caused by a someboot post-UEFI handoff.

## Axvisor Resolved Device Graph and Guest Firmware

Axvisor architectures build and resolve their own device graphs before final
guest firmware generation. The shared graph does not impose a common device
order: AArch64 still installs VGIC before IRQ consumers, RISC-V retains PLIC
hart/context setup, x86 retains LAPIC/IOAPIC/PIT/APIC-access ordering, and
LoongArch retains IOCSR and EXTIOI/PCH cascading.

When debugging a missing device or interrupt, follow one resource slot from
`DeviceModel::requirements()`, through `ResolvedDeviceGraph`, into both the
FDT/ACPI plan and `DeviceBuildContext`. Runtime devices must use the resolved
address and `IrqLine.input()`. The exact dyn model retained by the graph performs
the build, and the runtime seals only after every `ResourceClaimSet` slot becomes
a lease. For `console0`, first verify whether machine fallback, host FDT/ACPI
snapshot, or a same-ID user override supplied the final model and fixed binding.
MMIO/PIO exits must call the runtime optional-dispatch path once; a `find_*`
probe followed by a second dispatch indicates a stale routing path.

For x86 direct Linux boot, verify all of the following before changing Linux
command-line policy:

- the ACPI image fits wholly in `0xe0000..0x100000` and the RSDP is 16-byte aligned;
- `boot_params.acpi_rsdp_addr` contains that RSDP GPA;
- E820 reserves the ACPI image and the legacy low-memory window;
- RSDP/XSDT/table checksums and table pointer closure are valid;
- the MP table uses the same APIC plan for the explicit `acpi=off` fallback.

For x86 OVMF/BIOS boot, verify graph-fixed PIO windows `0x510..0x512` and
`0x514..0x51c` are trapped and that fw_cfg publishes `etc/acpi/tables`,
`etc/acpi/rsdp`, and `etc/table-loader`. A working selector read alone does not
prove ACPI installation; check table-loader DMA operations and confirm Linux
sees `DSDT`, `APIC`, `FACP`, and `XSDT` under `/sys/firmware/acpi/tables`.

For AArch64 host replacement, compare every GICR region and stride in the
immutable firmware plan with the `ArmVgicConfig` passed to the runtime. Do not
infer configuration by downcasting a registered GIC frontend. Host GIC MMIO
must remain trapped, and guest writes must never mutate host GICD/GICR state.

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
- Keep page-table phases separate while debugging: `someboot` owns boot-table formats and the MMU handoff, `ax-cpu` owns runtime stage-1 PTE/TLB semantics, and virtualization components own stage-2 formats. All may use `page-table-generic` as an execution engine, but that crate must not select the active architecture.
- Match the x86_64 dynamic UEFI path first: firmware disk layout, `to_bin` behavior, pflash/OVMF handling, and handoff expectations.
- Keep dynamic platform features aligned across `ax-std`, `ax-hal`, `ax-driver`, `axvm`, and the OS package. A partial `plat-dyn` feature set often compiles but fails after device or memory init.
- For std/musl targets, derive the initial JSON from a known Rust target where possible, then minimally adjust ABI, linker, relocation model, and soft-float. A `none-softfloat` target passing does not prove musl/std ABI correctness.
- Prefer runtime memory map data over board constants. Any early helper such as `phys_to_virt` must be valid for the phase where it is called.

## x86_64 Guest OVMF / Verified Upstream Firmware

Axvisor x86_64 boots a nested OVMF as the *guest* firmware through the axvm UEFI
path. The guest firmware is the upstream RELEASE OVMF built by Ostool
(`edk2-stable202508-r1`, fetched through `cargo xtask ovmf --arch x86_64` into
the `Source::LATEST` cache); its SHA-256 verification is done by Ostool itself.
The fixed profile is defined twice and the two copies must stay identical:

| Constant | Value | Defined in |
| --- | --- | --- |
| Profile name | `qemu_x86_64_axvisor_ovmf_debug` | `scripts/axbuild/src/axvisor/ovmf.rs::OVMF_PROFILE_NAME`, `virtualization/axvm/src/arch/x86_64/boot/mod.rs::FIXED_OVMF_PROFILE` |
| CODE GPA / size | `0xffc8_4000` / `0x37_c000` | `OVMF_CODE_BASE` / `OVMF_CODE_SIZE` |
| VARS GPA / size | `0xffc0_0000` / `0x8_4000` | `OVMF_VARS_BASE` / `OVMF_VARS_SIZE` |
| Combined size | `0x40_0000` (VARS followed by CODE) | `OVMF_COMBINED_SIZE` |
| Reset vector | `0xffff_fff0` | `OVMF_RESET_VECTOR` / `FIXED_OVMF_RESET_VECTOR` |

The upstream RELEASE layout matches this profile exactly (`code.fd` =
0x37c000, `vars.fd` = 0x84000, combined 4 MiB, reset vector at 0xfffffff0).
The verifier in `scripts/axbuild/src/axvisor/ovmf.rs` checks the firmware
directory layout (both `code.fd` and `vars.fd` present, correct sizes, reset
vector inside the CODE window); it never downloads anything. A local
unverified `code.fd` is accepted only through the explicit
`--allow-unverified-firmware` opt-in, must still match the fixed CODE size, and
has its SHA-256 printed for reference.

Debug commands (KVM hosts; SVM on AMD, VMX on Intel — pick the matching host):

```bash
# prepare the upstream firmware cache first
cargo xtask ovmf --arch x86_64
cargo xtask axvisor test qemu --arch x86_64 --test-group uefi \
  --test-case ovmf-entry-svm --firmware-bundle-path <ostool-cache-dir>
cargo xtask axvisor test qemu --arch x86_64 --test-group uefi \
  --test-case ovmf-entry-vmx --firmware-bundle-path <ostool-cache-dir>
```

The ostool cache dir is `$TMPDIR/ostool/ovmf/<arch>` (or overridden via
`TGOS_OVMF_DIR`), holding `code.fd` and `vars.fd`.

Wiring points to keep aligned:

- The case names are exactly `ovmf-entry-svm` / `ovmf-entry-vmx`
  (`test-suit/axvisor/uefi/ovmf-entry/qemu-x86_64-{svm,vmx}.toml`). The
  `--test-case` filter matches the full case name exactly, while the runner's
  pflash wiring matches the `ovmf-entry-` prefix (plus an exact
  case-directory check), so the variant suffix is load-bearing in both layers;
  a bare `ovmf-entry` is not a valid case name.
- The QEMU TOML sets `uefi = false`: `-kernel` still boots the Axvisor host
  (a PE32+ UEFI image), but `uefi = false` keeps `ostool` from injecting its
  own FAT ESP and lets the runner inject the verified upstream firmware
  instead.
- The runner injects `-drive if=pflash,format=raw,unit=0,readonly=on,file=<code.fd>` and
  `-drive if=pflash,format=raw,unit=1,file=<vars copy>` (fresh per run).
  Without these, QEMU falls through to SeaBIOS and hangs at `Booting from ROM..`.
- The nested OVMF is loaded by Axvisor itself from the VM config
  `os/axvisor/configs/vms/qemu/x86_64/ovmf-entry.toml`: `boot_protocol = "uefi"`,
  `firmware_profile = "qemu_x86_64_axvisor_ovmf_debug"`,
  `bios_load_addr = 0xffc8_4000`, `image_location = "fs"` /
  `kernel_path = "/guest/ovmf/OVMF_CODE.fd"` as the checked-in fallback (fails
  with a clear error, never silently uses a distro OVMF). With a firmware dir,
  the runner rewrites the generated VM config to `image_location = "memory"` and
  `kernel_path`/`uefi_firmware_path` pointing at the verified `code.fd`, which
  `os/axvisor/build.rs` embeds via `include_bytes!` — byte-identical to the
  QEMU-layer pflash CODE.
- The x86 loader (`virtualization/axvm/src/arch/x86_64/boot/mod.rs`) enforces
  `code_size = 0x37c000`, `load_gpa = 0xffc84000`, `reset = 0xfffffff0` only
  when `firmware_profile` is set; `axvmconfig` rejects any profile declared
  with a non-UEFI boot protocol (`FirmwareProfileRequiresUefi`).
- Guest fw_cfg access marker: `success_regex = ["(?m)^.*Nested OVMF fw_cfg accessed"]`.
  The nested OVMF firmware accesses fw_cfg (PIO 0x510/0x511/0x514) while
  booting inside the Axvisor guest; every such access is routed through the
  axvisor device model (`axdevice::fw_cfg::pio::FwCfgPioDevice`), which prints
  the `Nested OVMF fw_cfg accessed` marker at Info level. The marker is
  naturally scoped to the nested guest: the QEMU-layer host OVMF (injected as
  pflash to boot the axvisor host binary) drives fw_cfg inside QEMU itself,
  which never reaches the axvisor device model, so no VM anchor is needed and
  the regex is not subject to the 2048-byte streaming-DFA match window. The
  marker appears early (SEC/PEI), proving the nested OVMF executed; reaching
  BDS is a stage-2 concern (see the p2 milestone).

Common failures:

- No fw_cfg marker at all: wrong CODE layout (GPA/size mismatch, or
  `bios_load_addr` different from `0xffc84000`), or the run did not get the
  firmware — pass `--firmware-bundle-path` pointing at the ostool cache
  (`$TMPDIR/ostool/ovmf/x64`); without it the case fails fast with an error
  naming the flag.
- `uefi = true` trap: the case boots an `ostool`-managed OVMF with a FAT ESP
  instead of the runner-injected verified firmware, so the pinned layout is
  not what runs. Keep `uefi = false` and let the runner inject the pflash
  drives.
- VM config rejected at load: `bios_load_addr` not `0xffc8_4000`, CODE size
  mismatch, or `firmware_profile` on a non-UEFI boot protocol — the loader or
  `axvmconfig` validation errors name the expected value verbatim.
- The `uefi` group is non-gating: CI runs only the `smoke-svm`/`smoke-vmx`
  cases from the `normal` group, so validate `ovmf-entry-*` explicitly with
  `--test-group uefi`.

## someboot Startup Checklist

Use this order when auditing an early boot port:

1. Entry preserves firmware arguments and records them before BSS or relocation can destroy them.
2. Early serial output works before `ExitBootServices`.
3. Firmware memory map is captured, classified, and converted into the kernel memory model.
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

## RK3576 ROCK 4D Board Notes

The maintained ROCK 4D path uses the repository DTB and a U-Boot/firmware stack
that can load the StarryOS image and DTB through the board runner. Firmware must
implement the DTB's PSCI 1.0 `smc` method; otherwise secondary CPU release cannot
work even when the kernel image and CPU topology are correct.

- Use `os/StarryOS/configs/board/rock-4d.dtb`. Its root compatibles are
  `radxa,rock-4d` and `rockchip,rk3576`; do not silently substitute a U-Boot
  resident DTB from a different RK3576 board.
- The console contract is `serial0` / UART0 at `0x2ad4_0000`, 1,500,000 baud,
  8-N-1. The local template
  `os/StarryOS/configs/board/rock-4d-uboot.toml` uses `/dev/ttyUSB0`; adjust
  only the host serial path when the adapter enumerates differently.
- The DTB describes eight PSCI CPUs: Cortex-A53 MPIDRs `0x0` through `0x3` and
  Cortex-A72 MPIDRs `0x100` through `0x103`. Keep these hardware IDs separate
  from dense logical CPU indices. Both the maintained single-core board test
  and an eight-core boot have been validated.
- GICv2 CPU target bits are firmware/controller interface IDs, not dense
  logical CPU indices. Record each CPU's banked `GICD_ITARGETSR0` mask during
  per-CPU initialization and reuse that mask for SPI affinity, AxVM-assigned
  physical SPIs, and SGIs.
- The RK3576 CRU node must be `rockchip,rk3576-cru` at `0x2720_0000`, size
  `0x50000`. Early driver evidence should include
  `RK3576 CRU reg: addr=0x27200000, size=0x50000` followed by
  `RK3576 CRU clock/reset registered successfully`.
- The PMU parent must be at `0x2738_0000`, with a child compatible
  `rockchip,rk3576-power-controller`. Probe completion prints
  `Rockchip power-domain provider registered successfully`; an individual
  domain enabled at debug level prints `Rockchip power domain 0x... enabled`.
- The pinctrl provider must bind `rockchip,rk3576-pinctrl`, map IOC GRF
  `0x2604_0000` and optional SYS GRF `0x2600_a000`, and discover GPIO0 through
  GPIO4 from `gpio-ranges`. Successful registration prints
  `Rockchip RK3576 pinctrl registered successfully`. The generic rdrive FDT
  probe path applies SDMMC0's default `pinctrl-0` state before invoking the
  controller probe. Keep regression coverage at both boundaries: rdrive must
  prove default pinctrl is applied before the consumer callback, and the
  RK3576 pinctrl test must prove the ROCK 4D SDMMC state programs the expected
  IOC registers. Do not add a second consumer-specific apply call, and do not
  treat a later successful mount as sufficient evidence because U-Boot can
  leave the pads in a usable state.
- The repository ROCK 4D DTB connects SDMMC0 `vqmmc-supply` to the always-on
  RK806 `vccio_sd_s0` PMIC rail; it does not declare a fixed GPIO regulator for
  that controller. If a board DT variant uses a fixed GPIO supply, require
  `Fixed regulator ... enabled via pinctrl` (or an SD-specific regulator
  success log) before card initialization.
- The current CRU capability required by the storage path controls SDMMC0
  source/HCLK gates, source selection, divider, and reset. If kernel boot
  reaches storage initialization but cannot mount or access the root device,
  check CRU registration first, then SDMMC0 clock gating/rate and the relevant
  PMU domain before debugging the filesystem.

Use the exact board name exposed by the board service. List it first when
necessary:

```bash
cargo xtask board ls
```

The maintained single-core regression is:

```bash
cargo xtask starry test board \
  -c boot \
  --board rock-4d \
  -b Rock-4D
```

The validated eight-core path is:

```bash
cargo xtask starry board \
  -c os/StarryOS/configs/board/rock-4d.toml \
  --smp 8 \
  --board-config os/StarryOS/configs/board/rock-4d-board.toml \
  -b Rock-4D
```

Both runs must reach `root@starry:/root #` and print the standalone
`STARRY_ROCK4D_BOOT_OK` marker. A shell prompt without the marker is not a
passing board test.

Troubleshoot in handoff order: confirm the board service selected ROCK 4D,
confirm UART0 output at 1,500,000 baud, confirm the repository DTB compatibles
and PSCI method, confirm all intended MPIDRs were discovered, then check CRU
and PMU registration before investigating secondary release, storage, or
device-specific drivers.

## LoongArch Lessons

- On LS2K1000, repeated `failed to lock LS2K1000 LIOINTC when claiming LIOINTC IRQ` messages immediately after block hctx activation identify a hard-IRQ/controller-lock inversion, not a harmless spurious interrupt. Follow the AArch64 GIC pattern: keep the `rdif_intc` controller and its configuration registers task-owned, and publish a separate LIOINTC CPU interface containing only the ISR/domain/parent/atomic-enable state used by claim/complete. Looking up or locking the controller from hard IRQ lets a level interrupt continuously re-enter before the interrupted task releases its device guard.
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
- For guest-console failures, distinguish the host UART from the machine-owned guest UART. The host UART must never appear in a guest passthrough set. Check the fixed LoongArch guest resources (`ns16550a` at `0x1fe001e0`, PCH-PIC line 2), the generated FDT/SPCR/DSDT, the virtual PCH-PIC level state, and the Axvisor console mux before changing host IRQ routing.

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
