# AxVisor x86_64 Guest OVMF DEBUG Profile

The current guest firmware path fixes one reproducible OVMF DEBUG profile for entry diagnostics. It establishes the observable chain from firmware loading through SEC, PEI, DXE, and BDS. It does not implement pflash, a writable variable store, fw_cfg, PCI discovery, PM devices, or an OS boot source.

## Fixed firmware

The tgosimages asset is `qemu_x86_64_axvisor_ovmf_debug`. It is built from EDK2 tag `edk2-stable202605`, commit `b03a21a63e3bd001f52c527e5a57feddb53a690b`, with:

```text
architecture: X64
target: DEBUG
toolchain: GCC
platform: OvmfPkg/OvmfPkgX64.dsc
defines:
  FD_SIZE_4MB
  DEBUG_ON_SERIAL_PORT
  BUILD_SHELL=TRUE
  SMM_REQUIRE=FALSE
  SECURE_BOOT_ENABLE=FALSE
  TPM2_ENABLE=FALSE
  NETWORK_ENABLE=FALSE
  SDCARD_ENABLE=FALSE
  CC_MEASUREMENT_ENABLE=FALSE
```

`DEBUG_ON_SERIAL_PORT` sends diagnostics to COM1. Every checked-in x86 UEFI guest registers `x86-com1` at ports `0x3f8..0x3ff`.

| File | Size | Current use |
| --- | ---: | --- |
| `OVMF_CODE.fd` | `0x37c000` | Loaded at `0xffc84000..0xffffffff` |
| `OVMF_VARS.fd` | `0x84000` | Published template; reference QEMU and future writable-variable support only |
| `OVMF.fd` | `0x400000` | Must equal `OVMF_VARS.fd + OVMF_CODE.fd`; reference QEMU only |
| `manifest.toml` | n/a | Provenance, layout, features, markers, sizes, and hashes |

The reset vector is `0xfffffff0`. The current path maps only CODE. The `0xffc00000..0xffc83fff` VARS range is not loaded or emulated.

## Manifest contract

The manifest deliberately uses a flat TOML subset so setup can validate it without a host TOML
utility: every non-comment line must contain one unique bare key and one single-line string,
boolean, or unsigned integer value. Table headers, quoted keys, arrays, inline tables, and
multiline values are rejected. Required groups are:

- identity and provenance: `schema_version`, `profile`, `edk2_tag`, `edk2_commit`, `architecture`, `target`, `toolchain`, `platform`, `build_command`, `build_container_digest`, `tool_versions`, and `submodule_commits`;
- layout: `code_base`, `code_size`, `vars_base`, `vars_size`, `combined_size`, and `reset_vector`;
- files: `code_file`, `code_sha256`, `vars_file`, `vars_sha256`, `combined_file`, and `combined_sha256`;
- build switches: lowercase keys matching every define above;
- markers: `sec_marker`, `pei_marker`, `dxe_ipl_marker`, `dxe_core_marker`, and `bds_marker`.

The registry verifies the archive SHA-256. `scripts/ovmf-profile.sh` then verifies the manifest identity, fixed layout, individual file sizes and hashes, combined-file relation, reset-vector coverage, switches, markers, and provenance fields.

## Setup and unverified firmware

Normal setup pulls and verifies the fixed bundle automatically:

```bash
cd os/axvisor
./scripts/setup_qemu.sh nimbos-uefi
```

Distribution OVMF paths are not searched. A locally built same-layout CODE image requires both variables:

```bash
export AXVISOR_X86_64_UEFI_FIRMWARE=/absolute/path/to/OVMF_CODE.fd
export AXVISOR_X86_64_UEFI_ALLOW_UNVERIFIED=1
./scripts/setup_qemu.sh nimbos-uefi
```

The image must still be `0x37c000` bytes. Setup prints `UNVERIFIED` and the actual SHA-256.
`setup_qemu.sh` writes an `.unverified.generated.toml` VM config, while `quick-start.sh`
writes an `.unverified.toml` config. Test-suit build configs reference only the canonical
`.generated.toml` produced by verified setup.

`quick-start.sh setup` records the exact verified or unverified VM config it generated in a
per-guest `.selected` file. A later `run` reads that record instead of re-evaluating the current
process environment. Starting a new setup first removes the old record, and publishes the new
selection only after setup completes, so a failed setup cannot launch a stale configuration.

## Markers and diagnosis

The ordered firmware markers are:

1. SEC: `SecCoreStartupWithStack(`
2. PEI: `Platform PEIM Loaded`
3. DXE: `DXE IPL Entry` or `Loading DXE CORE at`
4. BDS: `[BdsDxe]`
5. OS: a marker owned by the selected EFI application or guest OS

Classify a timeout by its last marker:

| Last observation | Classification |
| --- | --- |
| no SEC | CODE mapping, reset vector, vCPU entry, or early serial |
| SEC or PEI | platform discovery or memory initialization |
| DXE | firmware device enumeration |
| BDS | boot source or guest OS handoff |

`ovmf-entry-vmx` and `ovmf-entry-svm` are non-gating diagnostics. After the fixed-layout CODE
image loads, the x86 diagnostic layer records a weak reference to that exact AxVM instance without
creating a matcher. Its first-run hook activates the matcher only after the VM registry contains
the same instance; prepare or registration failure and an unregistered duplicate VM ID therefore
cannot create or overwrite matcher state. The last vCPU exit removes the matcher, while a reset
reactivates it from the retained exact-instance qualification on the next first run. Repeated vCPU
activation is idempotent and does not reset a partially matched marker. Non-UEFI Guest COM1 output
does not create firmware-diagnostic state. Raw serial text is not a success condition because host
and guest firmware share the QEMU transcript. AxVM recognizes the complete SEC marker only while
processing writes from the enabled VM's emulated Guest COM1 and emits
`VM[1] guest COM1 reached OVMF SEC: SecCoreStartupWithStack(`; the diagnostic cases match that
VM-qualified boundary. `qemu-nimbos` never accepts the host message `VM[1] boot success`; its
eventual gate is the NimbOS-owned `usertests passed!` marker.

Use a Trace build to capture the first unsupported access after the last stage marker. Device traces include device name, PIO/MMIO direction, address, width, and value. An unregistered resource remains an error; the current path does not fabricate all-ones reads or discard writes.

## Reference qualification

Before publishing, tgosimages must prove reproducibility in two clean workspaces, verify all sizes and the combined-file relation, confirm the reset vector and required Shell/VirtIO modules, and run Q35 + TCG with SMM off, read-only CODE pflash, a working VARS copy, and a virtio-blk FAT ESP. The reference success marker is `AXVISOR_OVMF_REFERENCE_BOOT_OK`; the log must show SEC, PEI, DXE, BDS, then that marker. A second boot may change only the working VARS copy, never the published template.
