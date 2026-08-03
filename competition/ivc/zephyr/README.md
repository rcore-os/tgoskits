# Zephyr IVC control endpoint

This application is the RTOS half of the competition control loop. It targets
upstream Zephyr v4.3.0 on `qemu_cortex_a53`, receives the Rust `ivcproto`
CONTROL datagrams on UDP `10.0.0.2:5500`, applies each accepted actuator update
once, and returns STATUS followed by ACK.

## Guest network contract

The overlay enables the existing QEMU virt MMIO slot 16. The same values must
be used by the AxVisor VM configuration:

| Item | RTOS guest value |
| --- | --- |
| virtio-mmio base | `0x0a002000` |
| MMIO window owned by AxVisor | `0x1000` |
| DTS interrupt cell | GIC SPI 32 |
| Architectural guest INTID | 64 |
| MAC | `52:54:00:00:00:02` |
| IPv4 address | `10.0.0.2/24` |
| UDP endpoint | `10.0.0.2:5500` |
| L2 switch segment | 1 |

The AxVisor virtio-net configuration advertises MAC
`52:54:00:00:00:02` (`cfg_list = [2, 1, 1]`). The first two values select the
MAC suffix and switch segment. The final `1` explicitly selects the fixed
12-byte header compatibility mode. Upstream Zephyr v4.3.0 accepts
`VIRTIO_F_VERSION_1` and exchanges its modern 12-byte layout without accepting
`VIRTIO_NET_F_MRG_RXBUF`; the explicit mode pins that behavior independently of
feature-state tracking. Linux configurations omit this compatibility value and
use the negotiated legacy/modern layout. The overlay pins the Zephyr link
address to the same value, and startup treats any different runtime link
address as fatal. This also prevents an accidental slot/MAC mismatch from
producing misleading packet-loss results.

The Linux/Starry controller side is `52:54:00:00:00:01` and `10.0.0.1/24` on
the same isolated software-switch segment. No default route, NAT, shared-memory
transport, or hypercall data path is required.

## Build with upstream Zephyr v4.3.0

Install the Zephyr SDK supported by v4.3.0 and create an upstream workspace.
The commands below follow Zephyr's standard west workflow; `<repo>` is the
absolute path to this repository.

```sh
python -m venv .venv
. .venv/bin/activate
pip install west
west init -m https://github.com/zephyrproject-rtos/zephyr --mr v4.3.0 zephyrproject
cd zephyrproject
west update
west zephyr-export
pip install -r zephyr/scripts/requirements.txt

git -C zephyr describe --tags --exact-match
# expected: v4.3.0

west build -p always -b qemu_cortex_a53 \
  <repo>/competition/ivc/zephyr
```

The guest artifacts are `build/zephyr/zephyr.elf` and
`build/zephyr/zephyr.bin`. The board-specific overlay is discovered
automatically as `boards/qemu_cortex_a53.overlay`.

### Deterministic ACK-loss evidence image

The normal endpoint keeps both fault settings at their Kconfig default of zero.
Build the dedicated, finite fault image separately so it cannot be mistaken for
the normal demonstration image:

```sh
west build -p always -b qemu_cortex_a53 \
  -d <repo>/competition/ivc/zephyr/build-ack-loss \
  <repo>/competition/ivc/zephyr -- \
  -DEXTRA_CONF_FILE=ack-loss.conf
```

[`ack-loss.conf`](ack-loss.conf) selects 100 commands and suppresses only the
first ACK for every fifth freshly applied command. STATUS is still returned.
The controller's timeout therefore retransmits the same sequence; the duplicate
returns current STATUS plus ACK without applying the actuator or stepping the
plant again. Use the matching
[`linux-smp2-ack-loss.toml`](../config/linux-smp2-ack-loss.toml) and
[`zephyr-smp1-ack-loss.toml`](../config/zephyr-smp1-ack-loss.toml) VM
descriptions.

The fault run must contain exactly 20 `IVC-RTOS-INJECT` markers, the same 20
`IVC-RTOS-DUPLICATE` sequences, and one terminal `IVC-RTOS-RESULT` with 100
fresh applications, 20 suppressed ACKs, 20 duplicates, 120 STATUS frames, 100
ACK frames, and zero error/protocol-error frames. Validate the complete console
and bind the summary to its SHA-256 with:

```sh
python3 competition/ivc/analyze_qemu.py <qemu.log> \
  --output <summary.json> \
  --expected-count 100 \
  --profile ack-loss \
  --drop-ack-every 5
```

### Physical Orange Pi evidence images

The physical-board overlays keep the normal protocol behavior but make the
endpoint finite. `board-smoke.conf` accepts 20 fresh commands and `board.conf`
accepts 1,800. Both preserve the legacy `IVC-RTOS-RESULT` line and additionally
split terminal counters into compact `IVC-RTOS-OUTCOME` and
`IVC-RTOS-MESSAGES` records. Each compact record and the
`IVC-RTOS-POWEROFF` marker is emitted twice with a 10 ms pause before the guest
requests PSCI system-off, so the AxVisor board runner can regain control even
when the shared physical UART loses one span:

```sh
west build -p always -b qemu_cortex_a53 \
  -d <repo>/competition/ivc/zephyr/build-board-smoke \
  <repo>/competition/ivc/zephyr -- \
  -DEXTRA_CONF_FILE=board-smoke.conf

west build -p always -b qemu_cortex_a53 \
  -d <repo>/competition/ivc/zephyr/build-board \
  <repo>/competition/ivc/zephyr -- \
  -DEXTRA_CONF_FILE=board.conf

west build -p always -b qemu_cortex_a53 \
  -d <repo>/competition/ivc/zephyr/build-board-ack-loss \
  <repo>/competition/ivc/zephyr -- \
  -DEXTRA_CONF_FILE=board-ack-loss.conf
```

The third image is the physical 100-command ACK-loss campaign: it drops the
first ACK for every fifth fresh command and powers off only after all 20
deterministic retransmissions have been observed. Use these images only with
the matching `orangepi-5-plus-zephyr-*.toml` description.

After building and staging the matching StarryOS artifacts, run the physical
campaign from a clean worktree. The wrapper preserves every failed attempt,
harvests and hashes the raw CSV, validates all 20 injection/recovery pairs, and
restores board Linux after each repeat:

```sh
competition/ivc/run-orangepi-5-plus.sh \
  --profile fault-ack-loss \
  --repeat 3 \
  --require-clean \
  --result-dir competition/results/orangepi-5-plus/<campaign-id>
```
The normal QEMU image remains open-ended.

The retained physical build produced:

| Artifact | Bytes | SHA-256 |
| --- | ---: | --- |
| full `zephyr.bin` | 121,568 | `38c322b1181f09bde9dcb974bbffeaf576f8eac6dc97bd020a4e4ec831c3ec59` |
| full `zephyr.elf` | 2,179,208 | `b34f44fb22ba4d19a7160e3e30cfc8b17bcc1687398c63c436d4cf861cce5674` |
| smoke `zephyr.bin` | 121,568 | `d82d1f1a7a262a7f465990ce88ff7daa11c5034b68d82391efb10a5cddc61bb3` |
| smoke `zephyr.elf` | 2,179,416 | `a54130d6f217debbc8b28519a98b68bd618bb6010ad2d9a5c3c757e9fff200fd` |

`competition/ivc/analyze_board.py` accepts a run only when at least one
complete copy of each compact record exists, all complete copies agree, the
expected counts match, and StarryOS completion, Zephyr poweroff, AxVisor
filesystem sync, and restored board Linux are all present. The validated raw
logs and summaries are retained under
[`../../results/orangepi-starry-reference`](../../results/orangepi-starry-reference/).

The AxVisor image is built for non-secure EL1 (`CONFIG_ARMV8_A_NS=y`) and uses
safe GIC initialization so it does not reinitialize a distributor that the
hypervisor already owns. The raw binary must be loaded at `0x40000000` and
entered at the precise ELF entry, `0x4000100c`; these values are recorded in
`../config/zephyr-smp1.toml`.

### Validated build record

The target build was reproduced on Ubuntu 22.04.3 under WSL with Python
3.13.7, west 1.5.0, CMake 3.22.1, Ninja 1.10.1, DTC 1.6.1,
`aarch64-linux-gnu-gcc` 11.4.0, binutils 2.38, and ccache 4.5.1. The source was
the upstream `v4.3.0` tag (tag object
`981205b3e7cdf9fdf2e9e71b8b6b64fcc71c12a0`, commit
`3568e1b6d5cdd51a6b964a2a1d6d29200fea2056`). No external Zephyr modules are
needed by this application.

The Ubuntu Linux-target AArch64 compiler outlines atomics through a userspace
libgcc helper by default. `CONFIG_COMPILER_OPT="-mno-outline-atomics"` keeps the
Cortex-A53 guest self-contained. That toolchain also reports a missing
`include-fixed/limits.h` path literally; the validated build used a repo-local
compiler-prefix shim under `tmp/zephyr-toolchain` that redirects only that
query to GCC's existing `include/limits.h` and delegates all compilation and
binutils operations unchanged. The Zephyr SDK workflow above does not need
that host-packaging shim.

The final normal and fault layouts and hashes were:

| Artifact/property | Validated value |
| --- | --- |
| ELF entry / `__start` | `0x4000100c` |
| `PT_LOAD` virtual/physical address | `0x40000000` |
| `PT_LOAD` file size / memory size | `0x1dae0` / `0x7a000` |
| normal `zephyr.elf` | 2,170,024 bytes; SHA-256 `0643a85c9f999cc3780a4f57f9992262e535d2889d0b1d08b3dd1b544acfe7ac` |
| normal `zephyr.bin` | 121,568 bytes; SHA-256 `13b7bd6cca6398824a947cc7e038b996dd9a29227873bd065158e9873e723f68` |
| ACK-loss `zephyr.elf` | 2,170,920 bytes; SHA-256 `81749add8e14a4db9f3c2d388c07ba7f0f803242745b8c2bc4c2ee47b20227d4` |
| ACK-loss `zephyr.bin` | 121,568 bytes; SHA-256 `c2ea50effd0b1e910a88b75c6c57b89052269877867e1ef670ecdd20102d1550` |

The following non-secure native QEMU 10.0.3 smoke test checks the entry point,
timer/GIC state, slot number, driver, and guest-visible MAC. The timeout is
intentional because the endpoint serves requests indefinitely.

```sh
timeout 8s qemu-system-aarch64 \
  -cpu cortex-a53 -machine virt,gic-version=3 \
  -global virtio-mmio.force-legacy=false \
  -m 128M -nographic -no-reboot \
  -netdev user,id=net0 \
  -device virtio-net-device,netdev=net0,mac=52:54:00:00:00:02,bus=virtio-mmio-bus.16 \
  -kernel competition/ivc/zephyr/build/zephyr/zephyr.elf
```

It produced the `IVC-RTOS-SELFTEST PASS`, exact `IVC-RTOS-NET` MAC match, and
`IVC-RTOS-READY bind=10.0.0.2:5500` markers shown below. This is a native image
and device smoke test, not evidence of the AxVisor cross-guest UDP path.

The protocol and endpoint state machines can also be checked without a Zephyr
SDK. This compiles the same C sources with strict host warnings and exercises
the Rust-compatible golden vector, exact-once window, stale timestamp,
timeout-safe fallback, deterministic ACK-loss selection, duplicate no-reapply
behavior, and deterministic plant step:

```sh
bash competition/ivc/zephyr/run-host-tests.sh
# host-logic-tests: PASS
```

For an AxVisor boot, load the generated binary as the VM2/RTOS image and expose
the slot-16 virtio-net device with guest INTID 64 and config bytes `[2, 1, 1]`.
Do not write `64` into a GIC `GIC_SPI` device-tree cell: the cell is `32`, and
the interrupt controller adds the architectural SPI base of 32.

## Run the controller

After both guests have joined the same AxVisor L2 segment, the Linux/Starry
guest can run the existing controller:

```sh
cargo run -p ivcproto --bin ivcproto -- \
  controller 10.0.0.2:5500 1800 neural 100
```

The default endpoint has no finite request-count exit condition. Stop the guest
after the controller has collected its results. The physical StarryOS rootfs
autorun invokes the same checked-in binary with the profile count and neural
policy; the two physical-board overlays above intentionally power off after
their configured finite count.

## Compatibility behavior

The C implementation mirrors the Rust protocol currently in
`tools/ivcproto`:

- 32-byte little-endian `IVC1` version-1 header and IEEE CRC32, with the four
  checksum bytes treated as zero while calculating the checksum.
- 12-byte CONTROL, 20-byte STATUS, 12-byte ACK, and 8-byte ERROR payloads.
- A bounded 64-bit receive window. ACK carries the low 32 bits of the receive
  mask because that is the current Rust wire format.
- Fresh in-order commands update the actuator and deterministic thermal plant
  once. Duplicates return current STATUS and ACK without applying again.
- The optional ACK-loss evidence mode is disabled by default. When enabled, it
  drops only the first ACK for configured fresh sequences; retransmitted
  duplicates always receive STATUS and ACK.
- Fresh out-of-order commands are reported as `SequenceOutsideWindow`, matching
  the current Rust RTOS simulator even though the receive window records them.
- A valid-command silence interval greater than 500,000 microseconds enters
  safe mode with actuator 0 and fault `ControllerTimeout`.
- Guest monotonic clocks have unrelated epochs, so command age and the safety
  timer use the local receive timestamp, matching the Rust endpoint. The sender
  timestamp remains available to the controller for same-side round-trip
  measurement.
- The plant starts at 20 C and uses the same 100 ms model step, 20 C ambient,
  2.8 C/s heater gain, 0.04/s cooling coefficient, and steps 850 through 949
  disturbance as the Rust model.

At startup, the endpoint encodes and decodes the Rust golden frame (including
CRC `fe155dea` on the wire as `ea 5d 15 fe`) and the golden CONTROL payload.
Network setup starts only after this test prints:

```text
IVC-RTOS-SELFTEST PASS vector=rust-wire-v1
IVC-RTOS-NET mac=52:54:00:00:00:02 expected=52:54:00:00:00:02
IVC-RTOS-READY bind=10.0.0.2:5500 mac=52:54:00:00:00:02 window_bits=64 ack_loss_drop_every=0 expected_commands=0
```

Applied commands, duplicates, protocol errors, and timeout fallback use stable
`IVC-RTOS-* key=value` console lines so the demonstration harness can collect
them without parsing Zephyr's log prefixes. Finite physical images use the
redundant compact terminal records described above; the analyzer strips Zephyr
and AxVisor console prefixes before validating them.
