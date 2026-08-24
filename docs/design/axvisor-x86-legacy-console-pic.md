# AxVisor x86 legacy console PIC transport

## Problem

The ASUS NUC15CRH HTTP Boot firmware exposes the debug UART at the PC/AT COM1
I/O ports but provides neither an SPCR console description nor a usable AML
namespace. Runtime evidence shows received data pending in the UART IIR/LSR
and IRQ4 pending in the master 8259 IRR, while the corresponding IOAPIC GSI4
handler is never entered. The generic raw-console path therefore advertises an
IRQ-backed input capability that cannot wake the AxVisor management shell.

The affected user is the AxVisor board runner. After observing `axvisor:$`, it
must send `vm console 1` and observe the Linux guest success marker. The fix is
complete when the host shell consumes serial input without polling and the
existing board test completes through its shell-check step.

## Scope

This change adds an explicit legacy-PIC transport for the physical COM1
console. It does not add a general 8259 fallback, change guest PIC emulation,
or alter the x86 QEMU IOAPIC contract. The transport is selected only when the
build sets `AX_X86_LEGACY_PIC_CONSOLE=1`.

## Design

`x86-apic-driver` owns the register-level operations:

- initialize and remap the PC/AT PIC pair to vectors `0x30..0x3f` while every
  source remains masked;
- mask or unmask one legacy IRQ without disturbing other lines;
- acknowledge a master IRQ once, or a slave IRQ before its master cascade;
- configure the current CPU's LINT0 delivery mode as ExtINT while preserving
  firmware polarity and trigger fields.

`somehal` owns platform policy and runtime routing. With the explicit build
selection, it allocates a dedicated `X86LegacyPic` IRQ domain and maps ExtINT
vector `0x34` to controller-local IRQ4 without publishing an IOAPIC GSI
identity. Controller enable/disable targets PIC IRQ4 and BSP LINT0. Secondary
CPUs retain masked LINT0. An active ExtINT transaction completes the PIC before
issuing the ordinary local-APIC EOI.

The same explicit policy is applied while resolving an SPCR-derived ACPI route
for COM1. This keeps runtime UART adoption and raw-HAL input on the same
controller identity; without the opt-in, the ACPI route remains IOAPIC-owned.

`ax-runtime` keeps the device source masked until the framework handler and
controller line are enabled. This prevents an edge from being raised before
the consumer is ready.

## Failure handling

PIC initialization leaves all sources masked. Enabling first configures LINT0
and only then unmasks IRQ4. Disabling masks IRQ4 before masking LINT0. Invalid
legacy IRQs and LINT0 readback failures are explicit controller errors; the
runtime must not replace them with polling.

## Alternatives

- Polling was rejected because it fabricates a sleepable input capability and
  can starve management work under pinned vCPU load.
- Automatically attaching the guest was rejected because it makes the board
  test pass while leaving the host shell unusable.
- Enabling AML was tested and hangs while initializing the interpreter on this
  firmware.
- A generic PIC fallback was rejected because it would weaken the modern QEMU
  IOAPIC contract and could duplicate delivery on systems where both routes
  work.

## Validation

Deterministic tests cover PIC remapping, masking, EOI order, LINT0 ExtINT
encoding, raw-console activation order, and the board's explicit build
selection. The final hardware validation command is:

```bash
cargo xtask axvisor test board --board asus-nuc15crh-linux
```

Hardware validation is intentionally deferred until explicitly requested.
