# axloader

`axloader` is the UEFI loader used by AxVisor HTTP-boot boards. Its control
plane uses firmware-provided network protocols; serial is reserved for loader
diagnostics and for the target system after handoff.

The loader does not implement a network adapter driver and does not read UEFI
`ConIn` or open `SerialIo`. It selects one physical UEFI controller that
provides all of these protocols:

- `EFI_SIMPLE_NETWORK_PROTOCOL` for the permanent and current Ethernet MAC;
- `EFI_IP4_CONFIG2_PROTOCOL` for IPv4 configuration;
- `EFI_UDP4_SERVICE_BINDING_PROTOCOL` for server discovery;
- `EFI_HTTP_SERVICE_BINDING_PROTOCOL` for JSON control and image download.

Keeping those services in one interface bundle prevents discovery on one NIC
and HTTP transfer on another. Diagnostic text uses firmware `ConOut` only.

## Network boot protocol

The incompatible `httpboot-protocol` 0.2 flow is:

1. Configure IPv4 on the selected UEFI network controller.
2. Broadcast a JSON discovery probe to UDP port `2998`. The probe contains the
   protocol version, permanent/current MAC, architecture, and loader version.
3. Accept one server offer. Offers from different server instances are
   ambiguous and cause discovery to retry.
4. Read SMBIOS Type 1 identity and `POST /api/v1/loaders/poll` every two
   seconds while the device is unbound or bound and idle.
5. On a `boot` response, report progress to
   `POST /api/v1/loaders/status`, download the ELF, and verify both its declared
   length and SHA-256 digest.
6. Report `ready_to_handoff`, destroy UDP/HTTP/IP objects, call
   `ExitBootServices`, and enter the image.

Every loader restart performs discovery again and gets a fresh
`registration_id`. The server binds the device by its persistent MAC and may
reissue the active Session's same `boot_id`. A failed `boot_id` is not retried
until the server publishes a new command.

Discovery retries forever with a 1, 2, 4, 8, then 10 second capped backoff. An
unbound or idle loader remains available for configuration and future
Sessions; it never falls back to serial control.

## Hardware identity

The permanent SNP MAC is preferred. If it is empty, the current link MAC is
used. Only six-byte Ethernet addresses are accepted.

SMBIOS 3 is preferred and SMBIOS 2 is the fallback. The loader reports only
Type 1 manufacturer, product, version, and serial through HTTP. Parsing checks
entry-point checksums, structure bounds, string termination and string indexes,
and rejects tables larger than 1 MiB.

## Supported targets

| Architecture | Rust UEFI target | EFI boot filename |
| --- | --- | --- |
| `x86_64` | `x86_64-unknown-uefi` | `BOOTX64.EFI` |

The current loader accepts little-endian x86_64 ELF64 images. `PT_LOAD`
segments must have page-aligned physical addresses. If `httpboot_entry` is
requested, the loader resolves that symbol; otherwise it uses the ELF header
entry. The maximum download is 256 MiB.

## Build and test

Use the project task runner:

```bash
rustup target add x86_64-unknown-uefi
cargo xtask axloader build --target x86_64-unknown-uefi --release
cargo xtask clippy --package axloader
cargo xtask axloader test qemu --target x86_64-unknown-uefi
```

The output is:

```text
target/x86_64-unknown-uefi/release/axloader.efi
```

The QEMU test uses OVMF, q35, a virtio network device, real UDP discovery and
HTTP control/download. The serial stream is observed for diagnostics and is
never used to inject a command. Success requires all of the following:

- discovery and HTTP polling completed;
- `/kernel.elf` was requested;
- the declared SHA-256 was verified;
- `ready_to_handoff` reached the control server;
- `elf_loaded:` appeared in diagnostics.

## Install to removable media

The helper builds the loader, mounts an EFI partition, installs the removable
media filename, verifies the copy, syncs, and unmounts:

```bash
./bootloader/axloader/scripts/build-install-efi.sh
./bootloader/axloader/scripts/build-install-efi.sh --device /dev/sdb1
```

By default it finds the `OSTOOLBOOT` filesystem and installs
`EFI/BOOT/BOOTX64.EFI`.

## Troubleshooting

`network_select_error`

No single UEFI controller exposes SNP, IPv4 configuration, UDP4 service
binding, and HTTP service binding. Check that the firmware contains the driver
for the configured NIC.

`discovery_error: Timeout`

The loader did not receive a valid UDP offer. Check VLAN/bridge broadcast
forwarding, server UDP port `2998`, DHCP, and that exactly one server instance
is visible.

`control_boot_error`

The poll or status exchange failed. Check the offered HTTP base URL and the
server's `loader_network.public_base_url` as seen from the UEFI client. JSON
POST requests carry explicit `Content-Type: application/json` and
`Content-Length` headers because an HTTP/1.1 server must not infer a request
body from bytes following an unframed header block.

`elf_load_error: Download(SizeMismatch)` or `Sha256Mismatch`

The downloaded bytes differ from the active boot manifest. Upload a new
kernel, which creates a new `boot_id`; the failed command is intentionally not
retried.

When debugging handoff, remember that `ready_to_handoff` is the last reliable
network state. No UEFI network object may remain live across
`ExitBootServices`.
