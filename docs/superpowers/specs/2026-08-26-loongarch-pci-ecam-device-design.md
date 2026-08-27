# LoongArch Guest PCI ECAM Device Design

Status: Accepted

## Problem

AxVM's LoongArch UEFI guest firmware currently publishes a PCI root bridge and an MCFG window at
`0x2000_0000..0x2800_0000`, but the VM device graph has no runtime owner for that ECAM range. A
LoongArch Linux guest therefore reaches the PCI probe and faults on the first config-space access:

```text
VM[1] stage2 query miss: gpa=0x20000000
VM[1] VCpu[0] unhandled nested page fault at 0x20000000
```

The existing `pci=off` command-line workaround is not accepted by the tested LoongArch Linux
kernel. The failure occurs before the guest can discover and mount the configured virtio-mmio
block device.

The users are Axvisor developers and CI jobs that run LoongArch Linux guests with machine-owned
virtual devices. Completion means the firmware-published PCI configuration mechanism has an
equivalent runtime owner, Linux completes PCI probing without a nested-page-fault failure, and the
existing virtio-blk mount test reaches `VIRTIO_BLK_MOUNT_PASS`.

## Goals

- Model the LoongArch guest PCI ECAM window as a VM-owned runtime device.
- Make the runtime ECAM range and the ACPI MCFG/PCI0 range come from the same resolved device-graph
  resource.
- Return the architectural absent-function value for every currently unpopulated BDF.
- Keep the host PCI hierarchy and Axvisor's NVMe device inaccessible to the guest.
- Avoid allocating 128 MiB of backing RAM for the 128 MiB ECAM aperture.
- Preserve the existing virtio-mmio block device, PCH-PIC, fw_cfg, serial, and guest UEFI flows.

## Non-goals

- PCI endpoint passthrough, VFIO, IOMMU, MSI/MSI-X, BAR assignment, or DMA ownership.
- A general hot-pluggable PCI bus or public endpoint-registration API.
- Moving virtio-blk from MMIO transport to virtio-pci.
- Reproducing every QEMU `virt` PCI endpoint or exposing the host QEMU PCI bus.
- Changing AArch64, RISC-V, or x86 PCI behavior.

## Evidence and prior art

- AxVM's x86 path already follows the required ownership pattern. `X86PciConfigModel` owns the
  fixed `0xcf8..0xcff` PIO resource, contributes ACPI `PCI0`, and builds `X86PciConfigDevice` from
  the same graph node. The device returns all ones for an absent function.
- PCI Express Base Specification Revision 6.0.1, Section 7.2.2 defines the ordinary ECAM address
  mapping and access mechanism. PCI-SIG lists that approved revision on its
  [PCI Express Base specification page](https://pcisig.com/specification-overview/pci-express-base).
- PCI Firmware Specification Revision 3.3 is the current approved firmware specification. Its
  MCFG rules descend from Revision 3.2 Sections 4.1.2 and 4.1.3: MCFG communicates the ECAM base
  for boot-time segment groups and `_CBA` communicates a hot-pluggable bridge base. See the
  [PCI-SIG firmware specification page](https://pcisig.com/specification-overview/pci-firmware)
  and Linux's primary
  [PCI host-bridge ACPI guidance](https://docs.kernel.org/6.14/PCI/acpi-info.html).
- QEMU's PCIe host implementation decodes ECAM addresses and returns all ones when no device exists:
  [QEMU `hw/pcie_host.c`](https://gitlab.com/qemu-project/qemu/-/blob/472de0c851af86d26e3ccebf4154a27393091053/hw/pcie_host.c).
- The project's LVZ environment pins QEMU-LVZ commit
  `1d24ba8819fb5eb073dcaf2484e4634bb7b0d78f`; this environment confirms that the failure moves
  from hardware-capability detection to guest ECAM access when LVZ is available.
- The current QEMU LoongArch `virt` layout reserves `0x2000_0000..0x2800_0000` for ECAM. The AxVM
  LoongArch automatic MMIO pool starts at `0x3000_0000`, so the fixed ECAM aperture does not overlap
  automatically allocated virtual devices.

## Alternatives

| Approach | Result | Decision |
| --- | --- | --- |
| Identity-map host ECAM | Gives the guest authority to reconfigure host PCI/NVMe resources | Rejected: violates host ownership and isolation |
| `MapAlloc` zero-filled RAM | Zero vendor IDs create phantom PCI devices and writable fake state | Rejected: incorrect PCI semantics and wastes 128 MiB |
| `MapAlloc` prefilled with `0xff` | Avoids initial phantom devices but still wastes 128 MiB and makes writes mutate RAM | Rejected: a memory buffer is not a PCI config mechanism |
| Omit PCI0/MCFG | Correct for a permanently PCI-less machine but prevents current firmware topology from evolving toward PCI endpoints | Not selected for this change |
| Runtime ECAM device backed by the device graph | Firmware and runtime share the aperture; absent BDFs return all ones; no host exposure | Selected |

## Architecture

### PCI ECAM core

Add a focused PCI ECAM runtime device under `virtualization/axdevice`. Its initial topology contains
no endpoint functions. It owns one MMIO resource and implements byte, word, and dword config-space
accesses:

- subtract the resolved aperture base before decoding the ECAM offset;
- decode bus from offset bits `[27:20]`, device from `[19:15]`, function from `[14:12]`, and the
  4 KiB function-local register offset from `[11:0]`;
- return `0xff`, `0xffff`, or `0xffff_ffff` for an absent function;
- ignore writes to absent functions;
- reject unsupported widths and out-of-range accesses with typed `DeviceError` variants.

The aperture base must be 1-MiB aligned. Its size must be nonzero, an exact multiple of 1 MiB,
and at most 256 MiB. MCFG bus start is zero and bus end is `(size / 1 MiB) - 1`. Byte accesses are
always naturally aligned; word and dword accesses require natural alignment. No access may cross
the 4-KiB boundary of one function's configuration space.

The initial device is stateless and needs no lock. The implementation must not pre-design endpoint
registration, BARs, MSI, or DMA. Those capabilities require separate real consumers and design.

### LoongArch device model

Add a LoongArch-owned `DeviceModel` in `virtualization/axvm/src/arch/loongarch64`. The model:

1. requests the machine-policy ECAM aperture as a fixed MMIO resource;
2. contributes an ACPI `PciHostBridge` whose register slot is that same ECAM resource;
3. builds the PCI ECAM runtime device from the graph-resolved range;
4. rejects a resolved range that does not satisfy ECAM alignment, size, or machine-profile
   constraints.

The node is registered only for the LoongArch UEFI/ACPI boot policy. Direct-FDT guests keep their
existing plan and strict FDT validation; they do not gain an ACPI-only model that they cannot
describe.

`plan_devices()` registers this node alongside PCH-PIC, fw_cfg, and configured virtual devices for
UEFI guests.
The resource transaction therefore catches overlaps before VM construction, and
`PreparedDevices::build_planned` installs the ECAM device in `DeviceRuntime` before the guest runs.

### ACPI ownership

The LoongArch ACPI path will consume the resolved `PciHostBridge` special contribution instead of
constructing MCFG/PCI0 from an unrelated copy of the ECAM base and size. Machine-policy values for
the PCI MMIO window, I/O window, and INTx routing remain architecture-owned inputs, but the ECAM
register aperture comes from the resolved graph node used by runtime.

The PCI bus range is not an independent input: bus start is zero and bus end is derived from the
resolved ECAM size. The same derived range is used in MCFG and PCI0 `_CRS`.

The resolver must require exactly one LoongArch PCI host contribution with the expected identity
and one MMIO register. Missing, duplicate, malformed, or mismatched contributions fail VM creation;
there is no fallback to a guessed ECAM range.

The current LoongArch UEFI flow needs both a primary ACPI plan and an auxiliary FDT payload. The
framework will keep strict validation for the selected primary interface and add an explicitly
named partial resolver for auxiliary firmware contributions:

- strict ACPI resolution requires all UEFI runtime models, including PCI ECAM, to support ACPI;
- auxiliary FDT resolution includes every node that actually contributes FDT and deliberately
  skips the ACPI-only PCI ECAM model;
- the existing strict FDT resolver remains unchanged for direct-FDT guests;
- LoongArch UEFI special resolution expects the existing three shared FDT specials and four ACPI
  specials, with PCI present only in ACPI;
- no global firmware validation is weakened and no dummy PCI FDT node is generated.

Adding a direct-FDT PCI host description remains outside this change.

### Host firmware normalization

LoongArch machine planning will use one normalized PCI profile helper that reads host ACPI when it
is available and otherwise returns the QEMU `virt` defaults. `plan_devices()` constructs the fixed
ECAM model from this profile. `GuestPlatformBuilder::apply_host_acpi()` may still populate PCI MMIO,
I/O, and INTx windows, but after graph resolution it must compare its ECAM base and size with the
resolved ECAM resource.

An exact match is required. A mismatch fails VM creation with both ranges in the diagnostic; it is
never silently overwritten and never falls back to the host-derived or default value. After the
check, the resolved graph resource is assigned to `GuestPlatform.pci.ecam` and is the only ECAM
value consumed by MCFG and PCI0.

### VM-exit data flow

```text
Linux ECAM load/store
        |
LoongArch nested page fault
        |
DeviceRuntime MMIO lookup
        |
PCI ECAM device
        |
absent BDF => all-ones read / ignored write
```

The architecture VM-exit handler remains generic: it performs the existing runtime device lookup
and does not gain a hard-coded ECAM-address branch.

## Safety and ownership invariants

- No host physical PCI configuration range is mapped into the guest.
- The ECAM device never dereferences host MMIO and performs no DMA.
- The graph-resolved ECAM range is the runtime range and the ACPI MCFG range.
- The fixed ECAM range cannot overlap guest RAM, PCH-PIC, fw_cfg, or automatic virtual-device
  resources.
- Unknown BDFs never expose zero-initialized memory or retain guest writes.
- The guest cannot configure Axvisor's host NVMe device through this model.

## Error handling

- Invalid ECAM size/alignment or range overflow: typed device-plan configuration error.
- Missing or duplicate PCI host firmware contribution: VM creation error.
- Host-derived and graph-resolved ECAM mismatch: VM creation error containing both ranges.
- Unsupported Qword or cross-boundary config access: typed device access error.
- Device graph/runtime range disagreement: VM construction error before guest entry.

No failure silently falls back to host passthrough, allocated RAM, or address-specific handling in
the VM-exit path.

## Validation

### Deterministic tests

- ECAM address decoder covers bus, device, function, register, and boundary cases.
- ECAM validation covers zero, non-1-MiB-granular, over-256-MiB, misaligned-base, and overflowing
  apertures; MCFG bus start/end are checked exactly.
- Byte/word/dword reads for absent functions return width-matched all-ones values.
- Writes to absent functions do not change subsequent reads.
- Qword, overflow, and out-of-range accesses fail explicitly.
- The LoongArch model resolves exactly `0x2000_0000..0x2800_0000` and builds one runtime MMIO
  device.
- Device-graph overlap with guest memory or another fixed device is rejected.
- ACPI MCFG and PCI0 consume the graph-resolved ECAM register.
- Missing, duplicate, or malformed PCI host contributions fail firmware planning.
- UEFI mixed-interface resolution accepts three FDT specials and four ACPI specials without
  weakening strict direct-FDT validation.
- A deliberately mismatched host-derived and graph-resolved ECAM range fails VM creation, proving
  that host ACPI cannot remain a hidden second source.

Tests must compile and call the real Rust implementation; source-text assertions are not accepted.

### Integration and runtime

Run targeted format, clippy, AxDevice, AxVM, and firmware tests, then execute the LoongArch LVZ
container flow with the existing Linux mount configuration. Passing evidence requires all of:

```text
Hardware virtualization support enabled on core 0
VM[1] boot success
virtio_blk ... [vda]
VIRTIO_BLK_MOUNT_PASS
```

The run must not contain `unhandled nested page fault` for the ECAM aperture. An Axvisor exit code
of zero without the mount marker is not passing evidence.

The existing x86, AArch64, and RISC-V validation paths remain regression coverage for the common
device graph and virtio-blk implementation.

## Rollout and rollback

The behavior is machine-owned and enabled for LoongArch guests whenever the LoongArch firmware
publishes PCI0/MCFG. No user configuration or persistent format changes are introduced. Rollback is
the removal of the PCI ECAM model and restoration of the prior firmware input; existing guest
configs and virtio-blk image files remain valid.

Update `.claude/skills/arch-platform-porting/SKILL.md` in the implementation so the LoongArch PCI
firmware/runtime ownership contract remains synchronized with the code.
