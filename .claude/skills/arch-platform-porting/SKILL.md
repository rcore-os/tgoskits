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

## Runtime Console Before SMP

- When `ax-runtime` has both `irq` and `multitask`, initialize the scheduler and IRQ framework,
  probe UARTs, create owner-affine serial workers, and attempt the common runtime-console handoff
  before releasing any secondary CPU. Do not add a separate `serial` or `runtime-console` feature.
  Probe must leave every runtime dormant: registering a disabled controller IRQ is allowed, but do
  not mask, reset, or reconfigure any UART until that runtime is selected or explicitly opened.
- If no matching runtime UART was discovered, keep the raw HAL console as the sole owner and expose
  output through the same task-console capability; consumers must not maintain their own fallback
  state. Raw task input is valid only when the HAL console exposes an IRQ: the common layer drains
  RX into a bounded queue in hard IRQ and wakes a `WaitQueue`. Without a console IRQ, `take_input`
  must return unsupported and the consumer must disable its interactive entry point; do not fake a
  sleepable capability with `yield_now` polling or reserve a CPU through affinity. Raw task writers
  share a sleepable task lock and then an IRQ-safe hardware lock; log publishers use only a
  non-blocking attempt on the hardware lock because early AP and hard-IRQ records have no valid
  current task. Once a selected runtime enters `Preparing`, any failure must fail closed; after a
  successful commit, SMP code must never fall back to the early UART.
- Match a firmware-selected hardware `DeviceId` exactly. Only `NotSpecified` may fall back to the
  common Linux-compatible `ttyS0` numbering. A missing match before `Preparing` retains the raw HAL
  owner; it must not guess a different runtime UART. Fail closed only after a handoff has begun.
- Keep `begin -> adopt -> commit` and fail-closed policy inside `ax-runtime::console`.
  The exact firmware-selected console is already running, so adopt its line, baud, FIFO, and enable
  state without calling the ordinary serial `startup()` path. Mask only its device-local sources,
  enable the pre-registered IRQ action, then publish routing. This avoids iteration-count BUSY waits
  whose wall-clock duration changes between QEMU and a fast physical CPU. Non-console UARTs still
  run normal startup/configuration on their explicit open lifecycle.
  ArceOS, StarryOS, and Axvisor adapters must not maintain separate console-selection or handoff
  state.
- A secondary CPU may publish complete startup log records after installing its per-CPU area but
  before publishing a current task. Queue those records without waking the owner worker; publish
  the CPU's explicit log-wake readiness only after its scheduler, IRQ, and IPI paths are ready.
  The later scheduler-ready record sends the coalesced doorbell and drains the earlier records as
  one stream.
- Let each UART driver's bounded IRQ pass decide whether RX needs deferred rearm. Preserve an
  explicit rearm after an incomplete batch, overrun, exhausted IRQ budget, or runtime-ring
  overflow, but keep a fully drained RX source armed while the owner transports its samples;
  unconditionally masking a small FIFO until task context runs can overflow at line rate. If a
  deferred rearm immediately observes readable hardware while no IRQ samples remain, drain through
  the direct port path.
- Rearm `TX_SPACE` only while software bytes remain pending. Query FIFO/shift-register completion
  only inside the worker-owned `DrainTx` control transaction; a UART without a completion IRQ must
  let that transaction yield and recheck, not retain a false TX-ready state or create a timeout task
  that attempts to repair it later.
- Fatal output must make one terminal UART ownership transition. Use a bounded, non-sleeping claim
  so panic cannot deadlock on an interrupted normal transaction; after a successful claim, keep
  emergency ownership until shutdown, exclude worker/IRQ register access, and mask device-local
  sources before returning any writable emergency capability. The API must not represent a writable
  but unmasked state. Synchronously stream the complete formatted record instead of truncating it to
  a fixed software buffer.
- Axvisor must acquire the unique input before starting a vCPU and acquire the complete-record log
  subscription when the runtime backend supports it. Configure that owner before the startup banner
  or any ordinary host log. Host logs are independent lines during boot,
  shell-aware clear/log/redraw transactions in management mode, and bounded whole-record backlog
  while a guest owns the foreground console. Move its `TaskConsoleOutput` into one dedicated
  task before launching a vCPU. A virtual-UART/device callback can run inside the vCPU's pinned,
  preemption-disabled region, so it may only submit to a fixed-capacity, non-allocating queue under
  a non-sleeping lock and issue an IRQ-safe notification. Only the output task may wait on the
  sleepable runtime lock or UART backpressure. Report bounded-queue loss from that task; do not
  repair an atomic-context sleep with a watchdog, timeout task, affinity reservation, or platform
  polling fallback.
  Guest input queues must also report the first overflow as a complete host record and suppress
  duplicate reports until the guest drains input; never inject that report into guest UART bytes.
  Preallocate each guest-output ring before device callbacks can run, keep its hot path bounded,
  and report evicted byte counts before replay instead of silently overwriting old output.
- For a post-SMP interleaving failure, first verify the `runtime console active` message appears
  before secondary-CPU startup and that later log records reach the owner worker or Axvisor mux;
  do not weaken smoke-test fail regexes or reserve a polling CPU as a workaround.

## Porting Checklist

- **Target and toolchain**: add or verify `scripts/targets` specs, target triple, panic strategy, relocation model, code model, ABI, soft-float setting, musl/std support, linker, objcopy, and `rust-src` availability.
- **Kernel runtime mode**: keep the final image contract explicit. Starry is a freestanding
  `no_std`/`no_main` PIE built with `build-std=core,alloc`, always retains its SMP capability,
  and must not contain kernel TLS. Axvisor remains a std/musl PIE and explicitly enables TLS.
  ArceOS enables TLS by default, but configuration must reject `uspace + tls` rather than
  constructing an image with overlapping register ownership.
- **CPU-local execution-context ABI**: `cpu-local` owns current-source selection, context binding,
  switch transactions, and architecture-selected preemption state; `ax-percpu` owns only typed
  layout/storage. Do not create a second current-context per-CPU pointer. The active image
  mode determines the register assignment. The exact initialized `CpuAreaRef` address is the
  area identity; do not add in-image ABI versions, generation counters, cookies, provider-trait
  FFI, or raw TP access:

  | Architecture | CPU area | `LinuxCurrent` | `UnikernelTls` |
  | --- | --- | --- | --- |
  | x86_64 | GS base | GS runtime anchor; FS unused | GS runtime anchor; FS is TLS |
  | AArch64 | TPIDR_EL1/EL2 | SP_EL0; TPIDR_EL0 unused | SP_EL0; TPIDR_EL0 is TLS |
  | RISC-V | current-header backtrace or sscratch | `tp = current`, `sscratch = 0` | anchor current; `tp = TLS`, `sscratch = CPU base` |
  | LoongArch | r21 with KS3 mirror | `tp = current` | anchor current; `tp = TLS` |

  Keep LoongArch KS4/KS5 reserved for vCPU scratch. On RISC-V, `gp` is the ordinary global
  pointer again; target specs still need `--no-relax` where the PIE relocation model requires
  it, but must not describe `gp` as CPU-local storage.
  `CpuPin<'scope>` must be created only through the non-escaping guarded callback after checking
  the live CPU base, area self pointer/index, and current header. Atomic scalars require migration
  exclusion; shared `T: Sync` objects also rely on object-owned synchronization; mutable local
  objects additionally require `ExclusiveCpu` after excluding IRQ/re-entry and conflicting remote
  access. Context switches keep IRQs off and consume prepared/previous transaction tokens.
  AArch64 may lend SP_EL0 to userspace only after spilling the sole current header in the pinned
  kernel stack and must restore it before returning to Rust. Preemption tokens are linear.
  Load/store architectures retain the current-context owner across migration. x86_64 uses the
  CPU anchor, so a suspended switch guard resuming on another CPU must consume its old proof and
  adopt the equivalent depth left by the destination CPU's outgoing context. A runtime must claim
  its scheduler baton before releasing a final pending preemption depth; task policy and baton
  state do not belong in `cpu-local`. On a CPU-owned preemption architecture, a context's
  first-entry tail must finish the switch depth because it has no suspended incoming guard;
  context-owned architectures start that header at depth zero.
- **Build system**: wire arch/target mapping in `scripts/axbuild`, dynamic platform defaults, feature propagation, kernel format conversion, UEFI/to-bin behavior, rootfs handling, and per-OS test discovery.
- **QEMU and firmware**: verify QEMU binary, machine type, CPU, SMP count, pflash/OVMF files, serial console, disk/rootfs device, `-snapshot`, debug flags, timeout, and success/fail regexes.
  Obtain OVMF CODE/VARS through `cargo xtask ovmf --arch <arch>`, which reuses Ostool's pinned
  version, mirror probing, SHA-256 verification, and `$TMPDIR/ostool/ovmf` cache. Use
  `TGOS_OVMF_DIR` only to select another Ostool-format cache root; do not add per-consumer
  firmware variables or scan distribution-specific `/usr/share` candidates.
  QEMU `uefi`, `to_bin`, acceleration, CPU feature, and device choices are part of each
  `qemu-*.toml` contract; axbuild must not infer or overwrite them from the target architecture
  or host `/dev/kvm` availability.
  Axvisor x86_64 selects the VMX or SVM backend at runtime from CPUID; the generic QEMU board
  and all Axvisor build configs remain backend-neutral. CI must retain separate Intel/VMX and
  AMD/SVM QEMU cases because their host CPU exposure differs, but neither case may select a
  Cargo `vmx` or `svm` feature. Both cases must use the same backend-neutral guest baseline so
  their result isolates the runtime CPUID-selected virtualization path.
- **someboot arch layer**: implement or audit entry, relocation, BSS clearing, stack setup, memory map parsing, paging, trap vectors, timer, IRQ, power, SMP, and address translation.
- **CPU-local startup**: the final ELF carries exactly one `.percpu.template` plus
  `.percpu.init` and `.percpu.align`; it never carries a linked runtime area or compatibility
  alias. Discover the runtime CPU count, dynamically allocate every final area from that template
  geometry, construct all typed values once, freeze the layout, then bind each CPU with a
  `CpuAreaRef` while it is offline; only then may runtime code obtain a scoped `CpuPin`. Register
  publication must happen only after all fallible preparation succeeds. Dropping an uncommitted
  prepared-switch token must roll back the next task binding; the incoming tail must consume the
  previous binding epoch before that task can run elsewhere.
  AArch64 final aliases need cache maintenance consistent with their shareability attributes;
  RISC-V secondary boot must initialize `sscratch`; LoongArch must keep r21 and KS3 coherent.
- **CPU runtime**: update `components/axcpu/src/<arch>` for trap entry, context switch, user/kernel context, syscall return path, FP/SIMD state, and per-CPU assumptions.
- **Platform bridge**: update `platforms/axplat-dyn`, `platforms/somehal`, platform config, memory regions, IRQ routing, timer source, power operations, and CPU boot operations.
- **Scheduler-clock ownership**: keep comparable scheduler time in `ax-plat::time`, not in `ax-task` or an architecture trap module. `someboot` reports only whether its raw counter is synchronized across runtime CPUs; `axplat-dyn` initializes each bound CPU's clock anchor before scheduler/IRQ publication and stamps it from the local timer IRQ. On x86 SMP, an invariant TSC is not by itself proof of cross-CPU synchronization, so use the corrected per-CPU path unless boot code has established that proof. Remote readers may couple only the calling and target CPUs' published clocks; they must never substitute the calling CPU's raw TSC for a target sample. CPU-offline flow must close remote admission before withdrawing the target publication.
- **Runtime platform identity**: dynamic platform names should be discovered in `someboot`/`somehal` from firmware data, then exposed through `axplat-dyn` and `ax_plat::platform::platform_name()`. Keep `ax-hal` as a forwarding layer for platform identity, and keep static platforms returning `config::PLATFORM`.
- **Boot entropy**: capture trusted boot entropy in `someboot` before UEFI `ExitBootServices`. Prefer the UEFI RNG protocol, then an exact 32-byte FDT `/chosen/rng-seed`; expose the captured seed through `somehal`, `axplat-dyn`, and `ax-hal`. Consumers that require per-boot uniqueness must omit an optional interface or return an explicit unsupported error when neither source exists; normal capability absence must not panic, and consumers must never substitute monotonic time, counters, addresses, or other replayable state.
- **Runtime IRQ ownership**: ArceOS runtime IRQ traps are owned by `ax-cpu` and dispatched through `ax_hal::irq::handle_irq(raw_vector)`, which immediately wraps the CPU trap entry as `TrapVector`. `somehal` must stay OS-free and expose controller transactions through `somehal::irq::begin_irq(raw_vector) -> ActiveIrq`; `ActiveIrq::id()` returns the resolved `IrqId`, and `ActiveIrq` is held while `axplat-dyn` dispatches the IRQ and its `Drop` performs the architecture-specific EOI/complete. Do not reintroduce `_someboot_handle_irq` or `#[somehal::irq_handler]` as runtime dispatch glue.
- **Runtime IRQ initialization order**: dynamic platforms initialize boot IRQ state through `ax_hal::irq::init_boot_irqs(cpu_id)` before registering runtime IRQ handlers or probing normal devices. `rdrive::ProbeLevel` remains the coarse lifecycle boundary, and `ProbePriority` is the ordering source inside `PreKernel`: clocks first, then interrupt controllers, timer sources, MSI parent controllers, and only later normal early devices. For FDT, same level/priority matches must keep device-tree order; interrupt-controller nodes additionally follow parent-before-child ordering similar to Linux `of_irq_init()`, with sibling controllers preserving DT order. Do not add arch-specific ad hoc probe calls in `axruntime` when a priority barrier can express the same dependency.
- **Block runtime SMP order**: the IRQ-driven block runtime creates its control task and one CPU0 bootstrap hctx before secondary CPUs are required, so an early root filesystem remains usable. Only after every intended CPU has an online scheduler, working IPI delivery, and enabled local IRQ state may `axruntime` call `ax_fs_ng::block::runtime::online_smp()` once to add hctxs/vectors and rebuild per-CPU software submission channels. Drivers must not grow queues from secondary-CPU entry code, and an IRQ registration or queue-growth failure must unwind rather than switch to polling. Keep this contract synchronized with `docs/design/block-mq-runtime.md`.
- **Runtime IPI identity**: dynamic platforms expose the runtime IPI IRQ as a typed `IrqId` through `somehal::irq::ipi_irq()`, `axplat-dyn`, and `ax_hal::irq::ipi_irq()`. Do not route dynamic runtime IPI registration through `ax-config`; on RISC-V the IRQ is the flagged supervisor software interrupt cause in the CPU-local domain, not bare PLIC source `1`.
- **Runtime CPU limits**: treat generated `CPU_CAPACITY`/`SMP` as a build-time capacity for const generics and fixed-capacity scheduler structures, never as an instruction to replicate the ELF CPU-local template. Actual online/usable CPU count and dynamic CPU-area allocation must flow from the platform-discovered count, capped by `ax_hal::cpu_num()` where the OS capacity applies.
- **IRQ namespace rules**: keep CPU trap vectors, platform `IrqId { domain, hwirq }`, firmware sources (`IrqSource::AcpiGsi`, `IrqSource::AcpiGsiRoute`, explicit `IrqSource::ControllerLine`, and driver binding metadata such as `BindingIrqSource::FdtInterrupt`), controller-local hardware lines (`HwIrq`), and guest GSI/vector values in separate namespaces. New runtime IRQ registrations must use `IrqId`, not `usize`; legacy `IrqNumber(raw)` is only for static or still-unmigrated platform boundaries and must live in OS/HAL-facing layers such as `ax-plat`, `ax-hal`, or `axklib`, not `irq-framework` or `somehal`. `irq-framework` owns generic registry, affinity, execution, and boxed callback dispatch semantics; platform rebase work must preserve `BoxedIrqHandler`, `IrqExecution`, and `IrqRequest::new_boxed` while adapting the surrounding platform code to `IrqId`. `LEGACY_IRQ_DOMAIN` and `CPU_LOCAL_IRQ_DOMAIN` remain fixed compatibility domains, while dynamic `somehal` external controller domains such as GIC, PLIC, IOAPIC, EIOINTC, and PCH-PIC are allocated at controller probe time and must be reached through `alloc_irq_domain`, `domain_by_kind`, `domain_by_owner`, or `domain_is_kind`, not by constructing fixed numeric controller domains in dynamic-platform code. Do not derive a host IRQ with arithmetic such as `0x20 + gsi`, `PCI_INTX_VECTOR_BASE + gsi`, or by subtracting a trap-vector base in Axvisor/device code. Resolve firmware/device descriptions with `ax_hal::irq::resolve_irq_source(...)` / platform resolver and register the returned `IrqId`. When ACPI supplies trigger/polarity/controller metadata, carry it as `IrqSource::AcpiGsiRoute` instead of flattening it to a bare `AcpiGsi`, because PCI INTx routes may use a low GSI with non-ISA level/low semantics. Likewise, FDT device bindings should carry the raw interrupt specifier plus its controller owner in `BindingIrqSource::FdtInterrupt` until the OS/platform layer can resolve that owner to a controller domain and configure it; do not expose parentless FDT cells from `irq-framework` or configure a controller in generic driver probe merely to obtain a legacy number. `rdif_intc` controllers must expose fallible `translate_fdt` / `translate_acpi` methods that return controller-local hardware line and trigger metadata; the registering platform allocates or looks up a domain owner entry for the concrete `rdrive::DeviceId`, passes that domain to `rdif_intc::Intc::new(domain, driver)`, and the wrapper combines that domain with the local `HwIrq` before `configure` / `configure_acpi` programs trigger, polarity, vector, or mask state. Platform `irq_set_enable` and `irq_set_affinity` paths must route by the incoming domain's registered owner/kind and return an error on missing controllers, lock failures, unsupported affinity, or backend/type mismatches instead of silently no-oping. Empty, malformed, out-of-range, or unsupported firmware specifiers must return `IrqError` instead of IRQ 0, a base vector, or a guessed legacy number. If an FDT PCI host bridge preconfigures a controller-level legacy INTx route, store that route as a native `BindingIrq` source (plus any temporary raw compatibility value) and let child endpoints reuse it before falling back to PCI `interrupt-map` parsing.
- **Domain expectations**: x86 LAPIC timer and IOAPIC are distinct domains, so trap vector `0x20` is not `AcpiGsi(0)`. On aarch64, GIC INTID is the `HwIrq` within the GIC domain. On riscv64, PLIC source is the `HwIrq` within the PLIC domain. On loongarch64, EIOINTC and PCH-PIC must remain separate domains. A platform that cannot resolve an `IrqSource` must return `IrqError::Unsupported` instead of guessing a numeric IRQ.
- **x86 QEMU IRQ contract**: the dynamic x86 path targets modern QEMU `q35` with ACPI/MADT, Local APIC or x2APIC, IOAPIC, PCI INTx routing, and a physical-destination LAPIC MSI parent registered by `somehal` on the IOAPIC platform device. Do not add 8259/PIC fallback, i440fx-specific IRQ assumptions, non-ACPI IRQ probing, raw GSI enable bypasses, or vector arithmetic outside controller/provider ownership. LAPIC/x2APIC owns timer, IPI, EOI, and spurious handling; `X86IoApicIntc` owns external GSI route state, vector conflict checks, trigger/polarity, mask, and affinity updates through `rdif_intc::Intc`; `X86MsiProvider` owns reserved vector allocation, APIC destination messages, and parent-to-MSI-X leaf routes through `rdif_msi::Msi`. Fixed IRQ affinity must update the provider route before the PCI lease recomposes and reprograms the still-masked MSI-X entry. Hard IRQ dispatch may read only the pre-installed atomic vector route and generic IRQ route state; it must not look up `rdrive`. x2APIC paths must preserve full `u32` APIC IDs for CPU-local operations, while xAPIC, IOAPIC, and the current physical-destination MSI format must reject APIC IDs that cannot be encoded without truncation.
- **LoongArch QEMU IRQ contract**: the dynamic LoongArch path targets QEMU `virt`/LS7A-style firmware routing through CPU-local timer/IPI lines, EIOINTC, and PCH-PIC. `somehal::begin_irq(raw)` receives the CPU interrupt line from `ESTAT.IS`, not an ACPI GSI or PCI vector; only the timer line, IPI line, and EIOINTC cascade line may enter runtime dispatch. EIOINTC owns claim/complete of external vectors and enables its CPU-local cascade line during controller initialization; registering an Axvisor virtual IRQ injector must not re-enable that line or reinterpret it as `IrqSource::AcpiGsi(3)`. PCH-PIC owns PCH input state, ACPI trigger/polarity configuration, mask state, and route memory through its `rdif_intc::Intc` lock. Do not infer PCH-PIC input by subtracting `PCI_INTX_VECTOR_BASE`, do not treat ACPI `route.vector = PCI_INTX_VECTOR_BASE + gsi` as the EIOINTC hardware vector, and do not dispatch unknown CPU-local interrupt lines as PCH-PIC IRQs.
- **LoongArch LS2K LIOINTC contract**: follow the AArch64 GIC distributor/CPU-interface ownership split. The `rdif_intc` controller owns route and W1 enable/disable registers, while a separately published shutdown-lifetime CPU interface owns only the domain, ISR mapping, parent lines, and atomic enabled snapshot needed by hard IRQ claim/complete. Publish the CPU interface before enabling the parent cascade. Hard IRQ must not call `rdrive::get_list`, take a task-owned controller lock, allocate, or block. Publish enable after the controller's hardware write, and hide a disabled input from claim before disabling it in hardware.
- **RISC-V QEMU IRQ contract**: the dynamic RISC-V path targets QEMU `virt` firmware routing through CPU-local supervisor timer/software/external interrupt causes and one PLIC domain. `somehal::begin_irq(raw)` receives `scause.bits()`, not a PLIC source number; only S-timer, S-soft, and S-ext are runtime CPU-local causes. PLIC source IDs are controller-local `HwIrq`s and may only be produced by FDT translation or by claiming the PLIC after an S-ext trap. Do not dispatch a bare source number as a trap, do not treat PLIC source 0 as valid, and route PLIC enable through the registered `rdif_intc::Intc` controller instead of bypassing the rdrive lock.
- **RISC-V guest SBI IPI contract**: keep SBI decoding, hart-mask representation, completion ABI, and saved/live HVIP state in `riscv_vcpu`; keep guest hart-to-vCPU topology resolution in AxVM's RISC-V layer; and publish VSSIP through the architecture-neutral `VmInterruptSender` path. Validate the complete target set before publishing any interrupt. Current and remote vCPUs must use the same queued delivery path, and guest VSSIP must remain distinct from host S-soft runtime IPI and PLIC routing. Keep this contract synchronized with `docs/design/axvm-riscv-sbi-ipi.md`.
- **RISC-V guest CSR ownership**: `RiscvVcpu::setup()` constructs guest reset state without reading or writing live host `sstatus`/`hstatus`; the assembly guest-entry boundary is the only owner that swaps saved host and guest status. If the guest target exposes F/D, initialize HS `sstatus.FS` from that guest ISA contract while keeping the architectural guest FS state in `vsstatus`. A host `InstructionFault` immediately after returning from guest WFI points to a leaked host/guest status boundary; a later guest floating-point illegal instruction points to an inconsistent guest ISA/reset state. Do not repair either failure by copying current host CSR bits into the guest. Keep this contract synchronized with `docs/design/axvm-riscv-sbi-ipi.md`.
- **Runtime console selection**: Dynamic platforms expose the firmware-selected hardware console through `somehal::console_device_id()` and `ax_hal::console::device_id()`. The value is `Result<rdrive::DeviceId, ConsoleDeviceIdError>` derived from bootargs `console=`, ACPI SPCR, or FDT `stdout-path`; static platforms return `Err(NotSpecified)`. Linux-style `ttyS<N>` and `ttyAMA<N>` select the Nth ordinary FDT serial node, while Rockchip `ttyFIQ<N>` is a distinct `ConsoleSpec::RockchipFiq(N)` and must resolve against the Nth enabled `rockchip,fiq-debugger` node rather than aliasing an ordinary UART index. Numeric `tty<N>`, bare `tty`, and `ttynull` are virtual selections and must not bind a hardware device. OS code such as Starry should match `Ok(id)` against probed serial devices, use `ttyS0` as the Linux-style hardware-console fallback only for `Err(NotSpecified)`, and leave `/dev/console` unbound (`ENODEV`) for non-hardware console selections, unmatched selected hardware devices, or when no serial console TTY exists. Keep the console-spec parser and FDT node-to-`DeviceId` mapping together in `somehal`; do not reparse FDT or bootargs in the tty layer.
- **Runtime console ownership**: once Starry or another OS runtime binds the firmware-selected UART to an interrupt-driven tty/serial driver, claim both runtime output routing and the low-level platform output path through the serial-runtime ownership operation; do not leave those as separate caller-side transitions. The handoff enters `Preparing` to reject new early RX/TX/IRQ register access and wait for in-flight early access, and commits `Runtime` only after driver adoption, IRQ registration, and routing succeed. If a probed runtime UART fails after `Preparing`, enter `FailedClosed`; do not reinterpret that failure as an absent driver and fall back to raw HAL. Axvisor must consume the common task-console capability; the common layer matches an available runtime against the exact firmware-selected `DeviceId` and otherwise exposes the raw HAL backend without a second caller-owned fallback state. SBI or another console without a hardware serial runtime may retain synchronous raw output, but it must not expose task input as sleepable unless an IRQ-backed wake mechanism exists. Emergency takeover permanently masks only the UART's device-local IER/IMSC and must never disable a shared controller line. The hardware console must have one runtime register owner, and Axvisor task, guest, and log output must all pass through one application-level output owner; otherwise records and interactive redraw transactions can still overtake each other above the UART lock.
- **x86 raw-console IRQ fallback**: COM1 IRQ4 may share an IOAPIC line. Claim it only when UART IIR reports a pending, recognized RX or receiver-line-status interrupt; an absent port commonly reads as `0xff` and must be treated as unhandled even if the synthetic LSR bits look ready. Bound each hard-IRQ drain to the fixed software RX capacity so a broken or continuously-ready device cannot keep one CPU inside the handler forever.
- **Axvisor guest platform identity**: every Axvisor guest has a mandatory virtual UART with stable ID `console0`. Resolve it in the order machine fallback, valid host FDT/ACPI console snapshot, then user request with the same ID. FDT snapshots preserve node path, phandle, address span, register model/shift/access width, interrupt parent/specifier, clock providers, and stdout identity; ACPI snapshots preserve SPCR model, address space, range, IRQ, clock, baud, and namespace without retaining parser references or AML bytes. Guest TOML may replace `console0` model/options or add another serial ID, but must never provide numeric address, IRQ, controller, MSI/LPI, or `enabled = false`. A compatible model/transport keeps host fixed bindings and identity; an incompatible replacement becomes an automatically allocated virtual device. Keep exactly one host-console backend owner. Place vGIC distributor/redistributor at host GIC windows while retaining firmware identity. Firmware may describe padded, mutually overlapping GICv2 ranges; retain bases but normalize trapped apertures to the architectural 4 KiB Distributor and 8 KiB CPU-interface spans before registration. Corresponding physical UART and GIC ranges remain host-owned and excluded from passthrough. The application console mux remains the sole host-console input reader, and `SerialBackendFactory` creates a fresh generation on graph rebuild. A host-derived UART INTID remains a virtual controller input, not physical IRQ passthrough.
- **Axvisor virtual-device planning**: keep VM initialization order architecture-owned. Each architecture builds a `DeviceGraphBuilder` containing its controller, bus, host-replacement, passthrough, firmware-only, and configured virtual-device nodes. Graph nodes retain the exact `Arc<dyn DeviceModel>` used for `requirements()`, `firmware()`, and `build()`. User configuration is `id + model + typed options`, resolved by an explicitly populated `ConfiguredDeviceCatalog` whose entries are plain `ConfiguredModelRegistration` function pointers; do not add a factory trait, instance wrapper, device-type enum, linker registration, or firmware dyn container. Fixed requests come only from machine policy or normalized host firmware. Every architecture creates its own deterministic plan, registers its interrupt-controller bundle before IRQ/MSI consumers, issues one `ResourceClaimSet` per node, and seals `DeviceRuntime` only after every slot is retained by a lease. FDT/ACPI and runtime read the same `ResolvedDeviceGraph`. MMIO/PIO exits perform one runtime interval lookup and dyn `Device::access`; do not restore address/device special cases, downcasts, `find_*` plus second dispatch, a shared cross-architecture device sequence, a second interrupt fabric, or guessed controller/address/IRQ fallback. AArch64 creates one immutable plan before final DTB serialization; the same `ArmVgicConfig` and resolved resources drive VGIC construction, GICR/ITS nodes, and endpoints. Host-derived GICD, GICC/GICR, and ITS windows must be disjoint and non-overflowing. RISC-V retains PLIC hart/context ordering, x86 retains LAPIC/IOAPIC/PIT/APIC-access ordering, and LoongArch retains IOCSR/EXTIOI/PCH-PIC cascading. Keep this contract synchronized with `docs/design/axvisor-resolved-device-graph.md` and `docs/design/arm-vgic-interrupt-topology.md`.
- **Axvisor shared firmware providers**: replacing a host-owned FDT device also removes that device as a visible consumer of shared clocks, resets, power domains, or other providers. A passthrough guest must not retain unmediated write access that can disable the host-owned dependency. Preserve provider phandle/specifier/MMIO identity in the immutable machine plan, resolve a typed provider capability before VM construction, and attach a provider-specific `Arc<dyn DeviceModel>` as a `HostReplacement` node that claims the whole provider range so stage 2 traps it. The device may forward unrelated accesses only under provider-supplied, hardware-specific write-protection rules; the VM hot path must not contain board or SoC checks. Fixed providers without mutable registers need no mediator. Missing capability, ambiguous register layout, unsupported specifier shape, or invalid protection rules must fail VM creation instead of falling back to raw passthrough. Keep this contract synchronized with `docs/design/axvisor-shared-firmware-provider.md`.
- **AArch64 guest generic timer**: keep VM-wide frequency and counter offsets in immutable software state and give each vCPU separate CNTV/CNTP CVAL, ENABLE, IMASK, and loaded state. Record `CNTFRQ_EL0` on every enabled target pCPU and reject missing, zero, or heterogeneous values; an explicit valid host-FDT `clock-frequency` is the guest-visible firmware correction, not permission to skip hardware consistency checks. Hardware `CNTVOFF_EL2` is only a loaded execution-context copy. The final assembly entry window disables CNTV, installs offset and CVAL, executes ISB, and enables CTL; the exit window reads CTL/CVAL, disables CNTV, executes ISB, then clears CNTVOFF and restores host timer controls before any Rust code. Never read hardware CNTVOFF back into the VM-owned offset. Keep this contract synchronized with `docs/design/axvisor-aarch64-generic-timer.md`.
- **AArch64 guest timer PPI**: derive CNTV/CNTP PPIs and raw specifiers from the machine `GuestTimerProfile`, and publish only level state into the VM-local VGIC. Claim the host CNTV PPI once for the hypervisor lifetime and configure it level-triggered on every pCPU. On a lower-EL IRQ, assembly must read the GICv2 MMIO IAR or GICv3 `ICC_IAR1_EL1` while CNTV is still asserted; only then may the exit transaction disable CNTV, clear CNTVOFF, restore host timer controls, and call Rust to perform the priority drop. Reversing this order turns a level PPI into a spurious acknowledgement and re-entry loop. VGIC owns pending, active, enable, routing, EOI, DIR, and level re-pend. Under `hv`, every GICv2/GICv3 host CPU interface must use split EOI: `EOIR` only drops priority, while ordinary host `ActiveIrq::drop` performs the matching `DIR` and AxVM defers `DIR` until guest retirement. An acknowledged host CNTV PPI remains active until the corresponding GICv2 EOI/DIR or GICv3 LR/TDIR retirement reaches a typed backend operation after the controller lock is released; lowering CVAL/CTL before DIR must not deactivate it early. Hard IRQ only publishes fixed preallocated state and must not allocate, look up a VM, take `rdrive` locks, or invoke general subscribers. Do not carry IRQ state through an MPSC channel.
- **Dynamic IRQ route locking**: `somehal::irq::IRQ_ROUTES` is resolved from hard-IRQ paths, so every read and mutation must acquire its field-level IRQ-save lock. Do not use an ordinary spin acquisition for route lookup: local interrupt re-entry can recurse on the registry and stall every CPU waiting for the same lock.
- **AArch64 assigned physical SPI**: deliver an ownership-checked host SPI through a hardware-backed LR that carries the same guest/physical INTID. Normal guest retirement lets the physical GIC deactivate the source and must not repeat host `DIR` after software harvests the LR. A trapped guest DIR or teardown performs explicit host `DIR`; that operation, not a pre-DIR pending read, is the level-resample boundary. If a replacement acknowledgement arrives before a stale LR is harvested, retain it until refill can create the next HW-backed LR. Unassigned GICv3 interrupts, including ITS LPIs, remain host-owned: resolve the GIC parent through the registered MSI leaf route before dispatch and preserve the full 24-bit INTID for host `DIR`. This does not widen passthrough beyond validated SPIs.
- **AArch64 guest timer wait and migration**: WFI schedules the earliest deliverable CNTV/CNTP deadline. `Aarch64TimerBinding::wait_generation`, not `ArmTimerContext`, owns callback invalidation. After a callback wins the generation check, it must re-evaluate the captured timer snapshot against the current physical counter and publish the resulting PPI levels into the VGIC before waking the vCPU; waking first can let the vCPU observe no pending interrupt and sleep again indefinitely. A stale callback must neither publish nor wake. Keep VGIC queries and timer rearming outside the scheduler wait-queue critical section. Capture the VM runtime notification generation before those checks, recheck pending state after arming, and let the wait-queue predicate compare only lifecycle state and that atomic generation so a notification between the checks and blocking cannot be lost. Cancellation uses a `VmTimerHandle` containing its owner CPU; remote cancellation executes on that owner and rearms its comparator. Before vCPU migration, complete any CPU-local host timer activation on the old pCPU; reset/stop/drop call the binding invalidation transition, clear virtual lines, and advance the wait generation so stale events cannot become valid again.
- **Temporary host comparator guard**: `ax-task::begin_hardware_timer_irq` clears the recorded comparator deadline only after control enters the matching timer IRQ. This intentionally avoids rewriting an expired comparator before its pending event is consumed while the generic scheduler lacks separate programmed/pending/active comparator state. The guard can retain an expired deadline as the scheduling reference and delay later reprogramming; this performance cost is accepted temporarily. Remove it only after the IRQ acknowledgement path explicitly consumes comparator pending state without depending on comparator rewrite. Keep this exception synchronized with `docs/design/axvisor-aarch64-generic-timer.md`.
- **Dynamic firmware devices**: for `rdrive` ACPI probes, real non-empty ACPI ID lists enumerate namespace `Device` nodes and expose `_CRS` memory, I/O port, and IRQ resources through `AcpiInfo`; empty ID lists or synthetic root IDs are reserved for root-table style callbacks.
- **FDT child providers**: when a firmware transport owns protocol/provider child nodes, enumerate them through `FdtInfo::available_children()` or validate one with `FdtInfo::prepare_child()`, then publish with `PlatformDevice::register_fdt_child()` or the atomic parent-plus-child form `register_with_fdt_child()`. Do not publish capabilities to a raw phandle. Rdrive owns direct-child validation, disabled-node filtering, stable child `DeviceId`/path/populated state, duplicate and ownership errors, and retry-safe commit semantics. A child without a phandle may still be a valid protocol identity; phandle is only a consumer lookup key. Transport-specific code still interprets bindings such as SCMI protocol `reg`, and each published provider must retain its own backend/agent rather than route through an unrelated global singleton.
- **Page tables and memory**: check PTE flags, huge page support, direct map, kernel high map, MMIO map, TLB/cache barriers, and early `phys_to_virt` behavior before MMU state is fully recorded.
- **Page-table ownership**: `page-table-generic` owns only architecture-neutral walking, mapping, frame lifetime, structural PTE operations, and errors. It treats the caller-provided PTE configuration as an opaque associated type and must not define common permission or memory-attribute fields. Runtime stage-1 mapping flags, PTE formats, active-architecture metadata, virtual-address canonicalization, page-fault access flags, and TLB invalidation belong to `ax-cpu`; boot-table configuration and formats remain in `someboot`, and stage-2/EPT/NPT configuration and formats remain in their virtualization owner. `ax-hal` may provide the runtime `FrameAllocator` adapter and page-table aliases, but must not select architectures or define PTE bits. Do not reintroduce multi-architecture selectors under `memory/`.
- **Firmware address shape**: if firmware tables expose CPU-visible aliases such as LoongArch DMW addresses, canonicalize them through the architecture boundary before handing them to FDT memory setup, early console, or MMIO backends. Do not hide arch masks in generic `mem`/`common` helpers or duplicate them in drivers.
- **Runtime MMIO mapping contract**: keep `phys_to_virt` / `virt_to_phys` scoped to RAM direct-map translation. Device resource mapping must enter through `ax-mm::iomap()`, which asks `ax_hal::mem::prepare_iomap()` for an arch/platform decision before falling back to page-table-backed device mappings. Architecture-specific aliases such as LoongArch uncached DMW belong behind `someboot::ArchTrait::ioremap_device()`, not in `ax-mm` or drivers.
- **Drivers and rootfs**: check PCI command bits, MMIO/iomap, DMA address width, NVMe MSI-X/INTx routing, block device visibility, rootfs patching, and console/input feature flags. QEMU host root disks use NVMe and must not silently fall back to `virtio-blk`; guest virtual-device ABIs that bypass the host block runtime remain a separate scope.
- **StarryNixOS x86_64 diagnosis**: build the app-owned image natively from
  `apps/starry/nixos/flake.lock`, then run
  `cargo xtask starry app qemu -t nixos --arch x86_64`. Set
  `STARRY_NIXOS_REUSE_ROOTFS=1` only for an already-published image whose
  adjacent manifest passes lock, closure, target, ext4, and image-hash checks.
  Do not rebuild or switch the host NixOS system, or create repository-local
  caches outside `.ci-cache/{cargo,rustup,tmp}`. Diagnose the earliest ordered
  phase divergence and add a focused
  `qemu/system/<behavior>` case before changing syscall, procfs, mount, or
  filesystem semantics; do not treat later systemd fallout as the first root
  cause.
- **OS configs and test cases**: update ArceOS, StarryOS, and Axvisor configs only for validated architectures. Keep `qemu-<arch>.toml` runtime config separate from `build-*.toml`.
  Starry app board cases default to the matching
  `os/StarryOS/configs/board/<board>.toml`; add an app-local
  `build-<target>.toml` only when every board sharing that target can safely
  use the same CPU/MMU/SoC feature set.

## someboot Must-Haves

- Preserve the firmware entry ABI. UEFI entry carries `image_handle` and `system_table`; direct boot paths use different arguments.
- Establish an early console before risky transitions, then ensure a post-UEFI/post-MMU console path exists without Boot Services.
- Capture the memory map and kernel image physical range before address translation helpers depend on them.
- Treat relocated symbols carefully. After relocation or high-half switch, use runtime-safe symbol address helpers instead of raw compile-time addresses.
- For x86_64 direct PIE boot, apply supported `R_X86_64_RELATIVE` relocations in a naked,
  RIP-relative entry before executing Rust. The UEFI header may share the head section, but the
  raw entry symbol is the direct-loader entry and its physical address must remain valid after a
  load bias.
- On AArch64, pass EL transition state into the post-relocation entry path when it must be kept in Rust globals; do not write relocatable statics before relocation has been applied.
- On AArch64 UEFI entry, adapt `relocate::apply` to the generic EFI relocation
  contract, initialize the UEFI/FDT/ACPI state before common EL setup, and do
  not re-enter the direct-boot path that clears BSS or overwrites the captured
  FDT address.
- Clear BSS exactly once and after preserving any entry data that lives there.
- Render separate TLS and no-TLS linker layouts. A no-TLS kernel must reject `.tdata`/`.tbss`
  inputs and omit `PT_TLS`; a TLS kernel must retain the TLS program header and bootstrap data.
- On LoongArch OVMF, capture the EFI FDT configuration table as well as ACPI RSDP for firmware-described devices, but do not rediscover RTC in someboot/somehal through those tables. The dynamic UEFI RTC path should first use the UEFI Runtime Service `GetTime`; LS7A RTC nodes such as `loongson,ls7a-rtc` and ACPI `LOON0001` belong to the `ax-driver` fallback path when firmware RTC is unavailable.
- Allocate and align boot stack, per-CPU areas, secondary stacks, boot arguments, and page tables before enabling SMP.
- Install trap vectors before enabling interrupts, timer interrupts, MMU faults, or secondary CPU execution.
- On x86 QEMU, do not trust CPUID timing leaves unless the reported TSC frequency is plausible; some virtual CPU combinations expose invalid zero or tiny values. Prefer a trusted hypervisor timing leaf, then CPUID timing data, then PIT-based TSC calibration before falling back to processor base frequency.
- On x86 QEMU, initialize LAPIC/x2APIC once and keep APIC IDs as firmware IDs, not logical CPU indices. Use x2APIC MSRs when x2APIC is enabled, bound IPI delivery waits, reject xAPIC AP startup/IPI destinations above 255, and keep external IOAPIC INTx programming in the runtime `X86IoApicIntc` path instead of someboot or HAL bypass helpers. After a dependency-owned LAPIC enable sequence, explicitly mask and read back both LVT LINT0/LINT1 entries while preserving their delivery configuration; writing zero clears the architectural mask bit rather than setting it.
- On AArch64, keep the someboot `hv` feature scoped to the EL2 kernel path. For non-`hv` EL1 boot, choose the EL1 arch timer at runtime from the boot EL: use CNTP when EL2 is available and CNTV when EL2 is unavailable, and keep the FDT timer interrupt index consistent with the selected mode.
- On AArch64, program one-shot intervals through the 64-bit CVAL compare registers and a wrapping absolute deadline. Do not use the 32-bit TVAL registers when the interval may exceed their range.
- On AArch64 secondary entry, preserve the CPU metadata pointer explicitly across MMU-enable and EL-transition helpers. Naked asm should consume the helper return register instead of assuming scratch registers survive Rust calls.
- For RK3576 ROCK 4D firmware, DTB, serial, SMP, CRU/PMU, and board-validation
  requirements, follow the dedicated checklist in
  `references/boot-debugging.md`. Do not diagnose storage or secondary-CPU
  failures without first confirming that complete handoff contract.
- Build page tables for identity/firmware access, direct map, kernel high map, MMIO, and per-CPU data as the arch requires.
- Flush TLB/cache and use architecture barriers around page table writes, boot argument writes, and secondary CPU release.
- Treat hardware MMU enablement, direct-map/kernel-space addressability, and final kernel relocation as separate states. Generic relocation detection should use the final `VM_LOAD_ADDRESS`, not the broader arch kernel/direct-map range; for example AArch64 `hv` builds can use `PAGE_OFFSET = 0`, and LoongArch DMW can make RAM addressable before execution reaches the final high mapping.
- On AArch64, keep the SCTLR.M enable to relocated-entry branch window free of UART logging. Address helpers must not switch to relocated addresses while still executing on the pre-relocation path.
- On LoongArch, do not treat the DMW direct-map window as final kernel relocation. Address helpers may use DMW for early direct mapping, but relocated-kernel checks should only become true after execution reaches the final `VM_LOAD_ADDRESS` high mapping.
- After `ExitBootServices`, do not call UEFI Boot Services. Retry only through the correct memory-map-key sequence before exit.

## SMP Bring-Up Rules

1. Discover enabled CPUs from firmware data and keep firmware IDs separate from logical CPU IDs.
2. Bound-check CPU indices and avoid assuming hart/apic/mpidr/cpuid values are dense.
3. Prepare one boot argument block per secondary CPU with stack, page table, kernel entry, typed per-CPU area, and logical ID.
4. Flush boot arguments and page tables before the architecture CPU-on transport.
5. In the secondary path, initialize arch address windows, stack, page table state, the
   architecture CPU-local register contract, trap vectors, timer, and interrupt state before
   entering generic secondary code. Install the final `CpuAreaRef` while the CPU is offline, then
   obtain runtime access only through a scoped `CpuPin`; do not publish a raw base and initialize
   fallible state afterward.
6. Before the OS per-CPU register is initialized on a secondary CPU, use cached controller fast paths for interrupt and timer setup through `somehal::irq::init_secondary_boot_irqs(cpu_id)`; do not take `rdrive`, IRQ-domain, or generic route locks from that window.
7. Debug secondary failure with physical-address markers first; serial logging may not work until the secondary has its own mapping and trap state.
8. Keep architecture wake transport separate from generic CPU bring-up synchronization. The
   architecture hook only delivers PSCI/SBI/mailbox or INIT/SIPI; a shutdown-lifetime per-CPU
   state owned by someboot publishes `DEAD -> KICKED -> ALIVE -> SHOULD_ONLINE`. The secondary
   reports `ALIVE` after reaching the common someboot entry, then waits for the control CPU
   release before entering the OS. Keep this mutable state outside the copyable trampoline
   metadata and separate from the later scheduler-online publication. Do not replace it with a
   global last-CPU ID, an architecture-only completion, timeout retry, or scheduler-online flag.
   Expose this lifecycle through the non-blocking, non-copyable
   `start_secondary_cpu`/`status`/`release` handle: status observation must not release the CPU,
   and only a matching alive handle may release it. Claim the single in-flight transport with a
   non-blocking CAS. A dropped handle, upper-layer timeout, cancellation, or possibly partial
   transport error must keep the owner claimed because a late x86 AP may still consume the shared
   trampoline. Do not add cancellation, retry, fallback, or a someboot `Future`; a future upper
   layer needs a real waker and terminal cancellation semantics. Keep the 10-second synchronous
   polling policy in `axplat-dyn::PowerIf`, not in the someboot mechanism.
   On x86, encode SIPI as Linux `APIC_DM_STARTUP` (`0x600`); INIT level bits do not belong to SIPI.
   Keep this contract synchronized with `docs/design/someboot-secondary-cpu-startup.md`.

## Validation Ladder

Run the smallest useful check first, then climb:

```bash
cargo test -p axbuild --lib
cargo test -p axvmconfig
cargo test -p axdevice serial::tests
cargo test -p axvm machine::tests
cargo test -p virtualization-tests --test configured_device_graph
cargo xtask ktest qemu --workspace --arch <arch>
cargo xtask arceos test qemu --arch <arch>
cargo xtask starry test qemu --arch <arch>
cargo xtask axvisor test qemu --list --arch <arch>
cargo xtask axvisor test qemu --arch <arch> --test-group normal --test-case smoke
```

`ktest qemu --arch <arch>` filters the Cargo-metadata execution plan to packages
whose `[package.metadata.docs.rs].targets` contains the matching bare-metal
target. Packages without docs targets are intentionally x86_64-only, and
`[package.metadata.axtest] runtime = "board"` targets are excluded from QEMU.
Keep all ArceOS axtests for one architecture in one serial ktest invocation;
do not replace the separate ArceOS Rust/C suite command.

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
- NVMe block/rootfs missing: suspect PCI command enable, MMIO mapping, DMA address translation, MSI-X/INTx registration, bootstrap hctx startup, or rootfs patch path. Do not mask the failure by re-enabling `virtio-blk`.
- Axvisor only fails in LVZ container: verify container QEMU path, `cargo xtask ovmf --arch
  loongarch64` output (and `TGOS_OVMF_DIR` when set), target toolchain, KVM/LVZ flags, and whether
  xtask was built inside the mounted workspace.

## Completion Criteria

- The change is validated at the smallest affected layer and at least one end-to-end QEMU path for the target OS.
- Temporary debug markers, QEMU one-offs, and local-only paths are removed or documented as intentional.
- `qemu-<arch>.toml`, `build-*.toml`, and OS configs only advertise architectures that were actually validated.
- New target/container/firmware requirements are documented in the relevant skill, test-suit guide, or docs page.
- If the task changes architecture boot logic, someboot startup order, UEFI handoff, SMP bring-up, dynamic platform contracts, target JSON assumptions, or the recommended debugging flow, update this skill or `references/boot-debugging.md` in the same change.
