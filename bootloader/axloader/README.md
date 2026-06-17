# axloader

`axloader` is the UEFI loader used by AxVisor board boot flows. It is built for
one UEFI target architecture at a time, waits for a serial boot offer from the
host, then downloads and starts the AxVisor ELF image through UEFI HTTP
services.

The loader is intentionally small:

- the loader is built for one UEFI target architecture at a time;
- runtime boot metadata is sent by the host over the board serial console;
- the kernel image is fetched from the URL provided in that serial offer;
- ELF loading and entry selection are handled in the loader.

## Boot Flow

The normal flow is:

1. `cargo axvisor board` builds the AxVisor ELF for a board.
2. The ostool board server allocates a board session and exposes the ELF as an
   HTTP-accessible session artifact.
3. The user powers on or resets the board with `axloader.efi` installed on the
   EFI system partition.
4. `axloader` starts under UEFI and prints an `AXLOADER READY ...` line on the
   serial console.
5. The host sends an `AXLOADER BOOT ...` line containing the kernel URL, image
   size, architecture, image format, and optional entry symbol.
6. `axloader` downloads the ELF image, loads its `PT_LOAD` segments at their
   physical addresses, resolves the requested entry point, exits UEFI boot
   services, and jumps to the kernel.

The serial control protocol is shared through `httpboot-protocol`. The loader
depends on the workspace protocol crate with `default-features = false`, so the
UEFI binary can reuse the same string prefixes and protocol constants without
pulling in host-only functionality.

Board-specific boot metadata, including the kernel URL, image architecture,
and entry symbol, is supplied by the host in the `AXLOADER BOOT` offer. The
`board` field in `AXLOADER READY` is kept only for protocol compatibility.

## Supported Targets

Currently supported UEFI target:

| Target architecture | UEFI target | EFI boot filename |
| --- | --- | --- | --- |
| `x86_64` | `x86_64-unknown-uefi` | `BOOTX64.EFI` |

The x86_64 loader expects an x86_64 ELF image and prefers the `httpboot_entry`
symbol when the host provides it. If no entry symbol is provided, the ELF
header entry address is used.

## Build

Install the UEFI target once:

```bash
rustup target add x86_64-unknown-uefi
```

Build the loader by selecting the matching UEFI target:

```bash
cargo build -p axloader \
  --target x86_64-unknown-uefi \
  --bin axloader \
  --release
```

The output path follows Cargo's target directory layout:

```text
target/x86_64-unknown-uefi/release/axloader.efi
```

Host-side checks can run without a UEFI target:

```bash
cargo clippy -p axloader --all-targets -- -D warnings
```

The real loader build path should also be checked with the UEFI target:

```bash
cargo clippy -p axloader \
  --target x86_64-unknown-uefi \
  --bin axloader \
  -- -D warnings
```

## Install To A USB EFI Partition

The helper script builds the loader, mounts the EFI partition, installs the
loader under `EFI/BOOT`, verifies the copied file hash, syncs the device, and
unmounts it.

By default it looks for a filesystem labeled `OSTOOLBOOT` and installs
`BOOTX64.EFI` for the x86_64 UEFI target:

```bash
./bootloader/axloader/scripts/build-install-efi.sh
```

Use an explicit partition when needed:

```bash
./bootloader/axloader/scripts/build-install-efi.sh --device /dev/sdb1
```

Useful options:

```text
--target TARGET       Rust target, default: x86_64-unknown-uefi
--output FILE         EFI filename under EFI/BOOT, default: BOOTX64.EFI
--no-clean            Skip cargo clean before building
--keep-mounted        Leave the EFI partition mounted after writing
```

For removable media, make sure the board firmware can find the loader at:

```text
EFI/BOOT/BOOTX64.EFI
```

## Run With AxVisor Board Boot

After the loader is installed on the board boot media, run AxVisor through the
board flow:

```bash
cargo axvisor board \
  --config os/axvisor/configs/board/asus-nuc15crh-x86_64.toml \
  --board-config tmp/asus-nuc15crh-httpboot.board.toml \
  --vmconfigs os/axvisor/configs/vms/asus-nuc15crh/arceos-smp1.toml
```

The board config selects the remote board server and the board type. The server
allocates a concrete board session, publishes the freshly built AxVisor ELF,
and opens the configured serial console. Once `axloader` prints `AXLOADER
READY`, the host sends the boot offer over that same serial console.

Typical serial output starts like this:

```text
HTTP bootloader
round: 1/10
arch: x86_64
output: BOOTX64.EFI
serial_control_wait: waiting for AXLOADER BOOT
AXLOADER READY {"protocol_version":1,"board":"axloader","arch":"x86_64","loader_version":"axloader"}
```

After the host replies, the loader prints the selected boot metadata and the
download progress.

## Kernel Image Requirements

The current x86_64 loader accepts ELF64 little-endian x86_64 images. Loadable
segments must use page-aligned physical addresses because the loader allocates
UEFI pages at the segment physical load range.

The preferred AxVisor HTTP boot entry is:

```text
httpboot_entry
```

When the host sends `entry_symbol = "httpboot_entry"`, the loader resolves that
symbol from the ELF image and jumps to its physical address. Unsupported entry
symbols are rejected.

The loader enforces a maximum kernel download size of 256 MiB.

## Adding A New UEFI Target

To add a target, keep the target-specific data small and explicit:

1. Add the target architecture name and EFI output filename in `src/loader/mod.rs`.
2. Add the matching build-time validation entry in `build.rs`.
3. Choose the correct UEFI target and default EFI boot filename.

For example, a future LoongArch64 loader should use a LoongArch64 UEFI target
and the firmware-expected EFI boot filename for that architecture.

## Troubleshooting

`unsupported axloader UEFI target ...`

The selected UEFI target is unsupported or does not match its Rust target
architecture. Rebuild with the UEFI target shown by the error.

`control_boot_error: Timeout`

The loader did not receive an `AXLOADER BOOT` line before the serial-control
timeout. Check that `cargo axvisor board` is still waiting on the correct
serial device and that the board config selects the expected board.

`elf_load_error: Download(SizeMismatch)`

The HTTP download completed with fewer bytes than the host advertised. Check
the ostool server URL, network reachability from UEFI, and whether the current
session artifact is still active.

`elf_load_error: UnsupportedEntrySymbol`

The host sent an entry symbol that this loader does not implement. For the
current x86_64 AxVisor flow, use `httpboot_entry` or omit the entry symbol.
