# Boot Debugging Reference

This reference captures project-specific lessons from enabling LoongArch dynamic UEFI platform boot, someboot SMP, StarryOS tests, and Axvisor LVZ smoke testing.

## Layer Map

| Layer | Typical files | What must agree |
| --- | --- | --- |
| Target spec | `scripts/targets/**/<triple>.json` | ABI, soft-float, relocation model, linker, panic, std/musl support |
| Build orchestration | `scripts/axbuild/src/{build.rs,context,test/qemu.rs,*}` | arch to target mapping, features, UEFI mode, QEMU command, rootfs image |
| Test data | `test-suit/{arceos,starryos,axvisor}/**` | runtime TOML, build TOML, regexes, SMP count, firmware mode |
| Bootloader | `components/someboot/src/**` | entry ABI, relocation, memory map, paging, trap, SMP, power |
| CPU runtime | `components/axcpu/src/<arch>/**` | trap frame layout, context switch, FP/SIMD, user return |
| Dynamic platform | `platforms/{axplat-dyn,somehal}/**` | runtime memory/IRQ/timer/power facts from firmware |
| Drivers | `drivers/**`, `patches/virtio-drivers/**` | MMIO/iomap, DMA, PCI command bits, virtio transport |

When a boot failure appears in a high layer, still audit lower-layer contracts. For example, a Starry rootfs failure can be caused by PCI command bits, and an Axvisor hang can be caused by a someboot post-UEFI handoff.

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
4. Kernel image physical range, load offset, and high-half range are known before address translation helpers are used.
5. Page tables or arch direct-map windows cover the currently executing code, boot stack, page tables, kernel high map, MMIO, and boot data.
6. Trap vectors are installed using the address form required by the architecture at that moment.
7. MMU enable is followed by the required barrier, TLB flush, and an address-basis-safe jump.
8. Post-MMU console and panic paths are usable.
9. Per-CPU data and secondary boot stacks are allocated and initialized.
10. Immutable logical-to-hardware CPU metadata is complete before runtime CPU count is published; runtime IPI routing never reparses ACPI/FDT.
11. Each secondary's private interrupt interface and controller target are published before the CPU becomes online; a missing target fails bring-up.
12. Secondary CPU release happens only after boot arguments and page tables are visible to other CPUs.

## Runtime IPI Debugging

- Follow the ownership chain from queue/inbox publication to the final device
  write. The publish barrier belongs immediately before the LAPIC/GIC/SBI/IOCSR
  transaction, while the lowest public sender keeps an `IrqGuard` alive across
  identity lookup and commit. A guard only in a higher facade is insufficient
  if `somehal` still exposes an unguarded safe sender.
- Interpret `Success` as a committed doorbell, `Retry` as no new transaction
  committed, and `Invalid` as a permanent target/configuration error. On xAPIC
  and x2APIC, bound the busy wait before the ICR write; do not report `Retry`
  after a write that may already have delivered.
- For a software all-except-current send, keep one CPU pin across current-ID
  capture, complete target preflight, and every commit. Re-pinning for each
  target can migrate A to B, exclude old A, then mistake B for local and omit
  both CPUs. Preflight every encoding before the first write so `Invalid` never
  describes a partially committed broadcast.
- A scheduler/callback doorbell needs a generation token and a preallocated
  persistent retry set. When diagnosing a sleeping target, check the published
  work, claimed generation, retry bit/count, safe-point acknowledgement, and
  final WFI recheck in that order. A stuck retry may suppress WFI, but the idle
  loop must still enter the local scheduler on every iteration.

## RISC-V FDT SMP Notes

- Enumerate only CPU nodes that firmware marks available. A missing `status` property is usable, `status = "okay"`/`"ok"` is usable, and `status = "disabled"` must be skipped.
- Keep FDT `reg` hart IDs as firmware CPU IDs and map them onto dense logical CPU IDs separately. On VisionFive2, `cpu@0` is a disabled S7 management hart while the usable U74 cores are `cpu@1` through `cpu@4`; full-core boot should therefore start from hart 1 and bring up harts 2-4, not fall back to single-core mode.
- If a RISC-V board traps when secondaries are released, dump `/cpus` from the boot FDT before changing `max_cpu_num`; disabled or non-OS CPU nodes are a common cause of `cpu_on` targeting the wrong hart.
- Register ownership changes at one explicit platform boundary: naked boot entry captures the firmware `a0` hart ID in `CpuBootInfoV1` and puts a pointer to that typed record in `sscratch`; runtime entry replaces the pointer with `CpuAreaHeader*`. `sscratch` never contains the raw hart ID, `tp` is task TLS, and `gp` is always the standard psABI global pointer. If early CPU discovery observes a runtime header as a boot record, the call crossed that boundary in the wrong direction.
- After enabling the high virtual mapping, branch through a high-address naked trampoline that rebuilds `__global_pointer$` before calling Rust. Rebuilding `gp` before the address-basis change still leaves a physical alias in the register and commonly fails at the first relaxed global access.
- A guest or feature-probe assembly window may borrow `sscratch`, but every exit path must restore the host header before a per-CPU access, trap dispatch, or Rust call. Inspect the final object code because a source-level helper call inserted inside that window breaks the contract.

## LoongArch Lessons

- TLB refill entry and general exception entry use different registers and may require different address forms. Do not reuse a high-half virtual symbol where a physical TLB refill vector is required.
- Relocated symbols must be resolved relative to the running image. In the LoongArch SMP path, the secondary exception vector had to use a runtime symbol helper such as `sym_running_addr!(__exception_vectors)`, while the TLB refill entry needed the corresponding physical address.
- A secondary CPU can fault before it has a working serial path. Put markers before and after DMW setup, stack switch, page table register setup, trap-vector setup, and jump to the common secondary entry.
- Initialize trap vectors on every CPU, not only the boot CPU.
- Flush or barrier boot arguments before `cpu_on`; otherwise secondaries can observe stale stack, page table, or per-CPU data.
- Keep logical CPU ID mapping separate from firmware CPU IDs. LoongArch CPU IDs in firmware data are not guaranteed to be dense array indices.
- Treat live `r21 == KS3` as an invariant after the platform binder runs. KS0 is the trap stack, KS1/KS2 are trap temporaries, and vCPU code must use KS4/KS5 rather than borrowing KS3. Save user `r21` before loading KS3, and never restore a kernel trap frame's `r21` on return.
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
| Secondary CPU silent or primary CPU reports a pointer-like CPU ID during an IRQ storm | `cpu_on` argument, cache flush, stack, platform `CpuRegisterBinding` before secondary HAL/GIC/lock use, trap setup, logical CPU ID mapping; an unbound secondary can recurse through synchronous exceptions and overflow into an adjacent CPU area |
| Remote wake published but target sleeps | IPI generation/retry bit, publish barrier, immutable CPU-ID mapping, target interface readiness, final WFI gate |
| xAPIC callback corruption or wrong target | unguarded lowest sender, nested split ICR high/low writes, APIC-ID truncation, Retry reported after commit |
| ArceOS works but Starry fails | rootfs staging, std/musl ABI, console/input feature, tty assumptions, CPR sizing |
| Starry shell works but grouped tests fail | generated runner path, copied assets, success regex, `shell_init_cmd` versus `test_commands` |
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
