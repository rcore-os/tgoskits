# AxVM Host/Application Boundary

## Status

This design records the dependency boundary used by Axvisor and AxVM. The
change is behavior-preserving: it moves existing host operations behind a
narrow owner-provided API without changing VM configuration or device handoff
semantics.

## Problem

Axvisor is the application that selects VM policy and orchestrates VM
lifecycle. It previously depended directly on `ax-hal` for console and CPU
operations and on `ax-driver` for the x86 QEMU block-device INTx handoff. Those
dependencies exposed the implementation types of AxVM's ArceOS host adapter to
the application and allowed the VM lifecycle to bypass its owner.

Axvisor also exposed aliases for board-driver features. That duplicated
`ax-driver`'s hardware capability names and made it unclear whether the feature
belonged to the application or to its build configuration.

## Goals

- Keep Axvisor dependent on `axvm` for VM and VM-related host operations.
- Keep host HAL and driver types out of the Axvisor API surface.
- Preserve the single-reader host-console invariant.
- Preserve the x86 passthrough ordering and error behavior.
- Select board-driver features directly in build configuration without
  exposing driver APIs or duplicate feature aliases in Axvisor.

## Non-goals

- Making AxVM independent from its ArceOS host adapter.
- Defining a general-purpose public HAL or driver abstraction.
- Changing host-console polling into an interrupt-driven design.
- Generalizing the QEMU block passthrough profile to arbitrary PCI devices.
- Changing guest configuration syntax, IRQ routing policy, or VM startup order.

## Considered Boundaries

Keeping direct source-level access to the HAL and driver APIs was rejected
because it preserves duplicated ownership of host state. Exporting the complete
internal `HostCpu`, `HostMemory`, `HostTime`, and `HostPlatform` traits was
rejected because it would turn implementation-oriented runtime capabilities
into a broad public API. Adding raw HAL and driver wrappers to Axvisor was
rejected because it only renames the dependency without correcting ownership.

The selected boundary exposes only operations that the Axvisor application
must orchestrate:

- `axvm::host::console` controls and accesses the physical host console.
- `axvm::host::cpu` reports host CPU topology needed for console-reader
  placement.
- `axvm::host::x86` owns the fixed QEMU block-device IRQ route and handoff.

The public functions use AxVM types or plain data only. `ax-hal` IRQ types,
`ax-driver` PCI descriptors, and internal host traits remain private.

## Ownership and Lifecycle

Axvisor remains the policy owner: it decides when a filesystem-backed
passthrough VM requires host resource release and when guest startup may
continue. AxVM owns the mechanism and preserves this ordering:

1. After the VM is registered, AxVM resolves the host PCI INTx binding and
   installs the guest IOAPIC forwarding route and activation callback.
2. Axvisor requests host-filesystem shutdown through AxVM.
3. AxVM asks the host driver to prepare the QEMU block device for passthrough.
4. When the forwarding route activates during guest startup, AxVM unmasks the
   host INTx source.

Route discovery and device preparation remain best-effort and log unsupported
host configurations, matching the previous behavior. Failure to install an
AxVM forwarding route remains a VM initialization error.

The console API is task-context-only. Axvisor's multiplexer remains the sole
physical input reader, disables input interrupts, and polls one byte at a time.
The boundary does not introduce another buffer, reader, or IRQ callback.

## Feature Ownership

Axvisor board and test configurations select nested `ax-driver/<feature>`
features directly. The optional Axvisor dependency is a Cargo feature-routing
anchor: it becomes active only when a configuration selects one of those
driver features, while Axvisor source code never calls the driver API. AxVM
separately enables its x86 PCI driver dependency for the `host-fs` capability
that performs the QEMU block-device handoff.

## Validation

The change is validated by checking that Axvisor's default graph has no direct
dependency on `ax-hal`, `ax-driver`, `ax-kspin`, `ax-sync`, or `spin`; building
Axvisor for x86_64, AArch64, and RISC-V; running targeted clippy checks; and
exercising the Axvisor x86 QEMU/axtest flow where the environment provides the
required guest image.
