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
10. Secondary CPU release happens only after boot arguments and page tables are visible to other CPUs.

## RISC-V FDT SMP Notes

- Enumerate only CPU nodes that firmware marks available. A missing `status` property is usable, `status = "okay"`/`"ok"` is usable, and `status = "disabled"` must be skipped.
- Keep FDT `reg` hart IDs as firmware CPU IDs and map them onto dense logical CPU IDs separately. On VisionFive2, `cpu@0` is a disabled S7 management hart while the usable U74 cores are `cpu@1` through `cpu@4`; full-core boot should therefore start from hart 1 and bring up harts 2-4, not fall back to single-core mode.
- If a RISC-V board traps when secondaries are released, dump `/cpus` from the boot FDT before changing `max_cpu_num`; disabled or non-OS CPU nodes are a common cause of `cpu_on` targeting the wrong hart.

## AArch64 Guest Bring-Up Lessons

- Assembly offsets into a `repr(C)` vCPU context must include the alignment of every embedded type, not only the sum of preceding field sizes. Lock offsets such as guest system registers to `core::mem::offset_of!` regression tests; an eight-byte padding mistake can make one register restore corrupt an unrelated register such as `TPIDR_EL0`.
- Decode PSCI IDs using the architectural 32-bit `0x8400_0000..=0x8400_001f` and 64-bit `0xc400_0000..=0xc400_001f` ranges. Implement `PSCI_VERSION` explicitly before forwarding unknown calls so a guest can discover the supported PSCI contract.
- Prefer the selected guest DTB as the source of its architectural virtual-timer PPI. When a guest binary uses a compile-time device tree and AxVM receives no external DTB, set `[devices].aarch64_virtual_timer_irq` explicitly and validate that it is a PPI (`16..=31`). A board guest with only emulated devices should also use `interrupt_mode = "emulated"`; leaving an empty-passthrough configuration in passthrough mode can expose an unrelated host-private PPI such as the physical timer IRQ.
- A hardware-backed virtual timer list register transfers physical PPI completion to the guest. Keep the current-vCPU scope installed through deferred IRQ work, and do not deactivate the physical interrupt until the guest EOI path completes it.
- Quiesce guest timer sources on every AArch64 VM exit before clearing the current-vCPU scope or scheduling host work, while retaining the saved `CNTP/CNTV` compare and control state for the next entry. On an IRQ exit, first acknowledge the level PPI and transfer it into the hardware LR; only after the VM-exit handler returns may the local `CNTP_CTL_EL0` and `CNTV_CTL_EL0` sources be disabled. Disabling them immediately after saving registers can withdraw the level PPI before GIC acknowledgement and leave the guest stuck during IRQ initialization. Restore `CVAL/CTL` on the next guest entry, require the structured `unowned_virtual_timer_irqs` count to remain zero, and keep any defensive stale-source cleanup free of synchronous UART output; generic diagnostics must be bounded or rate-limited.
- A hardware-backed virtual-timer PPI is owned by the physical CPU on which the vCPU programs it. Do not give that vCPU a multi-pCPU affinity mask unless migration also transfers or re-arms the timer state and physical PPI ownership. A baseline that shares pCPUs with host work should keep each vCPU pinned and set `dedicated_cpus = false`; otherwise an absolute sleep can remain blocked after the vCPU migrates, which measures a broken timer route rather than scheduler contention.
- `dedicated_cpus` is an AxVM placement contract, not global host isolation. The partition planner removes reserved pCPUs from other shared guest-vCPU affinities; it does not automatically move ordinary AxVisor tasks, housekeeping, or physical IRQs. Prove any claimed isolation with observed pCPU accounting and the actual affinity of the interference source.
- For a controlled AxVisor host-interference experiment, configure the bounded task explicitly in the AxVisor build profile, start it after VM initialization but before `start_default_vms`, wait until its singleton affinity is observed, and stop/join it after the default VM exits. Persist the requested and observed masks, safety deadline, stop reason, loop count, and coverage window in the host trace. A loop-local counter interval includes time when the task was preempted, so label it as observed pCPU wall time rather than exclusive task CPU runtime; use scheduler/pCPU accounting for utilization claims.
- Do not assume round-robin makes two AArch64 vCPU tasks safe on one pCPU. Deferring host preemption until an architecture guest run slice returns avoids a nested-current-vCPU panic, but OrangePi-5-Plus testing still produced a current-EL data abort immediately after switching to the second singleton-pinned vCPU. Before using same-pCPU vCPU contention as benchmark evidence, add a deterministic alternating-vCPU regression and pass a physical-board smoke with no nested-vCPU marker, current-EL exception, state corruption, or serial binary output.
- Treat multi-pCPU vCPU affinity as a separate migration test. A run that allows a vCPU on pCPU1/pCPU3 and later emits an unexpected physical PPI followed by current-EL aborts is a correctness failure, not a latency outlier. Use singleton affinity in formal evidence until both architectural state and timer/PPI ownership migration are covered by regression tests.
- The AArch64 hardware-timer injector runs in hard-IRQ context. Detect the GIC backend and publish any GICv2 hypervisor MMIO endpoint before registering that injector; neither hardware-timer nor software LR injection may look up or lock an `rdrive` GIC device. Software injection runs with the current vCPU installed and can be preempted by that vCPU's timer PPI, so holding the same device lock creates a same-CPU recursive deadlock. A repeatable guest freeze whose request count moves earlier when LR logging is added is a strong sign of this widened lock/preemption window.
- Measure direct virtual-timer injection latency in one guest-visible counter domain. At the host PPI entry, read `CNTPCT_EL0`, subtract `CNTVOFF_EL2` modulo 2^64, and record the translated tick with the target vCPU and injection result; at the guest timer handler entry, record `CNTVCT_EL0`. Validate `CNTFRQ_EL0` on both sides before offline pairing. Both IRQ paths must use preallocated, lock-free records with no printing or allocation, and must report dropped, incomplete, failed-injection, and frequency-mismatch counts. If host pCPU utilization is reported, close the architectural-idle interval before IRQ dispatch so handler time is counted as running rather than idle.
- On permanent vCPU stop, disable both `CNTP_CTL_EL0` and `CNTV_CTL_EL0` on the same physical CPU before unbinding the vCPU. A VM-wide stop initiated by one vCPU must also run idempotent per-task cleanup on every other vCPU's assigned physical CPU, because those tasks can observe the stop only after a recoverable exit has already unbound them. Do not apply this cleanup to ordinary WFI iterations; an enabled stopped-guest timer otherwise becomes a host PPI storm.
- Virtio MMIO device-configuration reads must pack the requested byte, word, or dword width in little-endian order. A byte-reading guest can hide this bug while a dword-reading guest observes a truncated MAC address.

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

## Axvisor Physical-Board Shutdown Handoff

- When an Axvisor board build enables `fs`, use the shell's `shutdown` command before an external reset or power cycle. Wait for the exact `AXVISOR_HOST_FILESYSTEM_SYNCED` marker; it confirms that cached host filesystem state was written and block IRQ registrations were released before the platform powers off.
- Treat `AXVISOR_HOST_FILESYSTEM_SYNC_FAILED:` as a hard stop. Preserve the serial log and do not remove power automatically, because the host filesystem could still contain dirty state.
- The Axvisor shell does not interpret shell operators such as `;`. Board automation must send `shutdown` as a standalone command rather than a Linux-style `sync; ...` command line.
- Never expose the same physical root partition to a guest after Axvisor mounts it. Use an independent guest image or device so the host and guest cannot mutate one filesystem concurrently.

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
