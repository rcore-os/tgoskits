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

`httpboot-protocol` 0.3 按同一 `boot_id` 交接内核和可选的宿主
`initramfs`、`cmdline`。`BootPayload` 在退出 UEFI Boot Services 前安装到配置表，
由 someboot 接收并预留归档物理页。会话流程如下：

1. 在选定的 UEFI 网络控制器上配置 IPv4，并向 UDP 端口 `2998` 广播包含协议版本、
   MAC、架构和加载器版本的 JSON 发现报文。
2. 只接受一个服务端的响应；多个服务端响应会触发重新发现。
3. 读取 SMBIOS Type 1 身份；设备未绑定或空闲时，每两秒调用
   `POST /api/v1/loaders/poll`。
4. 收到 `boot` 后通过 `POST /api/v1/loaders/status` 报告进度，下载内核 ELF，
   核验长度和 SHA-256。若本会话提供宿主 initramfs，再下载该归档并独立核验长度
   和 SHA-256，同时保存 `cmdline`。任一核验失败都不交接。
5. `PreparedPayload::publish()` 把归档页和命令行写入 `BootPayload` 配置表；随后
   报告 `ready_to_handoff`，销毁 UDP/HTTP/IP 对象，退出 Boot Services 并进入内核。
   若状态上报失败，撤销配置表并回收尚未交接的归档页。

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
- `/kernel.elf` 和 `/session/initramfs.cpio` 都被请求；
- 内核和归档各自的长度及 SHA-256 均已核验；
- 诊断输出包含 `host_payload_ready:`；
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

`elf_load_error: Download(SizeMismatch)`、`Sha256Mismatch` 或 `host_payload_error`

内核或宿主归档与当前会话清单不符，或者宿主镜像无法安装到 UEFI 配置表。检查
服务端该 `boot_id` 的镜像和 `cmdline`，上传新制品以创建新 `boot_id`；失败的命令
不会在同一会话中自动重试。

When debugging handoff, remember that `ready_to_handoff` is the last reliable
network state. No UEFI network object may remain live across
`ExitBootServices`.
