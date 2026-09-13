# loongarch-intc-driver

`loongarch-intc-driver` is a `no_std`, OS-independent register driver for the
Loongson EIOINTC, PCH-PIC, and LIOINTC interrupt controllers.

The crate owns hardware protocol only:

- EIOINTC IOCSR initialization, vector enable, claim, and W1 completion;
- PCH-PIC trigger/polarity, local mask, HTVEC route, and immutable
  EIO-vector-to-input mapping;
- LIOINTC parent routes, W1 enable/disable, and a lock-free enabled snapshot
  shared with its hard-IRQ CPU interface.

Firmware discovery, `ioremap`, IRQ-domain allocation, `rdrive` registration,
parent-cascade sequencing, handler dispatch, and `ActiveIrq` completion policy
belong to platform glue such as `somehal`.

## Endpoint model

Each constructor returns separate task and hard-IRQ endpoints:

```rust,ignore
let parts = EioIntcParts::new(iocsr, EioIntcConfig::new(256)?)?;
let controller = parts.controller;
let cpu_interface = parts.cpu_interface;
```

The same pattern is provided by `PchPicParts` and `LioIntcParts`. PCH and LIO
receive caller-mapped `mmio_api::MmioRaw`; EIO receives an `IocsrAccess`
capability. `NativeIocsr` is available on LoongArch targets, while host tests
can inject a fake backend. The mapped PCH/LIO apertures use typed
`tock-registers` layouts internally, so register offsets and volatile access
remain private hardware details.

Production `IocsrAccess` implementations are also hard-IRQ capabilities: their
64-bit read/write operations must be bounded and must not sleep, allocate,
take a blocking lock, or call back into OS services.

The crate intentionally has no `dma-api` dependency: these interrupt
controllers do not own DMA memory, device-visible addresses, or cache
coherency transitions. A DMA capability should only be added with a real DMA
data path and ownership contract.

The `rdif` feature implements `rdif_intc::Interface` and `DriverGeneric` for
all three controllers. Translation returns controller-local `HwIrq`; the
platform attaches a domain through `rdif_intc::Intc::new`.

## Safety and ownership

- The caller must keep every `MmioRaw` mapping valid for all returned endpoint
  lifetimes and externally serialize task-context controller operations. PCH
  and LIO constructors reject mappings that are too short or not naturally
  aligned for their typed register blocks before creating register borrows.
- The crate never maps physical memory, probes firmware, acquires an OS lock,
  or calls another controller.
- PCH parent EIO sequencing is deliberately outside the crate.
- LIO enable writes hardware before Release publication; disable hides the
  input with AcqRel before the hardware write. Its hard-IRQ endpoint never
  borrows the task-owned controller.

See [`docs/design/loongarch-intc-driver.md`](../../../docs/design/loongarch-intc-driver.md)
for the complete topology, compatibility contract, Linux v7.1 prior art, and
validation plan.

## Validation

```bash
cargo test -p loongarch-intc-driver
cargo test -p loongarch-intc-driver --all-features
cargo xtask clippy --package loongarch-intc-driver
```
