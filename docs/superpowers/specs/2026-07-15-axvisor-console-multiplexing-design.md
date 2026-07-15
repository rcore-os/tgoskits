# Axvisor AArch64 VM Console Multiplexing Design

## Goal

Selectively port the VM console multiplexing behavior from upstream commit
`5c794bcae087ba3460704ad507f9dc920bcff343` onto `hybrid-mode-dev` without
cherry-picking the commit or replacing the existing AArch64 Hybrid interrupt
forwarding implementation.

An AArch64 VM with one configured emulated console must expose that console to
the guest, allow `vm connect <VM_ID>` from the Axvisor shell, forward host input
to the VM, print guest output on the host console, and return to the shell when
the user presses `Ctrl+]`.

## Scope

The port includes:

- emulated PL011 and 16550 MMIO UART devices in `axdevice`;
- registration and lookup of one connectable AArch64 console per VM;
- guest-FDT console nodes, `/chosen/stdout-path`, `/aliases/serial0`, and
  console-related kernel command-line rewriting;
- VM APIs for console discovery, input delivery, output draining, and virtual
  IRQ notification;
- authorization of emulated console SPIs in the virtual GIC distributor;
- Axvisor shell `vm connect` handling, input switching, output pumping, and
  `Ctrl+]` detach behavior.

## Non-goals

- Do not cherry-pick the upstream commit.
- Do not replace the existing PPI 27 forwarding logic, SPI ownership table,
  Hybrid route discovery, or hardware-LR injection rules.
- Do not change RISC-V, LoongArch, or x86 interrupt-mode behavior.
- Do not change the checked-in default AArch64 Linux VM configuration as part
  of this port.
- Do not support multiple simultaneously attached host shell sessions.
- Do not add pCPU time-sharing or vGIC context migration.

## Architecture

### UART devices

`axdevice` owns the UART register models. Both devices use bounded RX and TX
FIFOs protected by the repository's non-sleeping lock. Host input enters the RX
FIFO, guest transmitter writes enter the TX FIFO, and the UART status and
interrupt-identification registers are derived from FIFO and interrupt-enable
state.

Each FIFO holds 4096 bytes and drops the oldest byte on overflow, matching the
upstream behavior. PL011 requires an MMIO aperture of at least `0x1000` bytes;
16550 requires at least 8 bytes. A missing subtype value selects PL011 for
compatibility, while `cfg_list[0] == 1` selects 16550. Other explicit subtype
values are rejected instead of silently selecting a different model.

The VM device collection records at most one connectable AArch64 console. A
second configured console is rejected as invalid configuration rather than
silently replacing the first console.

### Guest description and interrupt boundary

For each configured AArch64 console, guest FDT generation adds the appropriate
PL011 or `ns16550a` node using the configured GPA, length, and full GIC INTID.
The node uses a GIC SPI specifier, so INTIDs below 32 are rejected. The primary
console updates `/chosen/stdout-path`, `/aliases/serial0`, and removes stale
`console=`/`earlycon=` arguments before appending the selected UART arguments.
Arguments after the first `--` remain init arguments and keep their order.

When the physical host UART is listed in `excluded_devices` and the emulated
console reuses its GPA, excluded-device reservation generation must skip the
overlap with the emulated device. This prevents the reserved stage-2 range from
blocking the emulated UART mapping. Other excluded ranges remain reserved.

The console SPI is an emulated interrupt. It is authorized in the virtual GIC
for guest configuration, but it is never inserted into the physical Hybrid SPI
ownership table and never routed through the physical IRQ injector.

The virtual GIC authorization set is the union of canonical AArch64 Hybrid
guest SPI routes and emulated console SPIs. Only canonical host-FDT physical
routes enter `SPI_OWNERS`, receive host affinity, or reach the physical IRQ
injector. Console INTIDs must be in `32..1020`; PPI 27 remains a separate
current-vCPU hardware-LR path and is not part of either SPI set.

### VM and shell data flow

Host input follows:

`shell -> AxVM::push_console_input -> emulated UART RX -> virtual IRQ -> guest`

Guest output follows:

`guest UART TX -> emulated UART TX -> shell output pump -> host stdout`

`vm connect <VM_ID>` accepts only a running AArch64 VM with a connectable
console. While connected, shell line editing is suspended and input bytes go to
the VM. `Ctrl+]` detaches locally and restores the Axvisor prompt. If the VM is
removed, stops accepting console I/O, or returns a console error, the shell
detaches and restores the prompt.

The connected state contains both VM ID and a monotonically increasing
generation. Input and output paths detach with a compare-and-replace of the
exact state token. Only the path that successfully clears that token prints the
returned message and prompt. This prevents an old output-pump iteration from
detaching a newer connection, including a reconnect to the same VM.

Every input push and output drain verifies that the VM is still running. The
output pump snapshots the state token before looking up the VM, and rechecks
that token when detaching. A VM stop, removal, or console error therefore ends
only the matching connection.

The output pump must not hold a VM/device lock while writing host stdout. It
drains into a bounded stack buffer, releases the device lock, then writes.

## Error handling

- Invalid UART range, unsupported MMIO width, invalid GIC INTID, or duplicate
  console configuration returns a typed error.
- Console input to a VM without a console returns `NotFound`.
- Console IRQ delivery first pulses the VM interrupt fabric. If no fabric
  backend exists or the pulse fails, it directly injects the same INTID into
  vCPU 0. A successful fallback completes the input operation; if fallback
  injection fails, its typed error is returned.
- Shell errors are reported to the user and do not panic or leave the shell in
  connected state.

## Testing and validation

Use TDD for each behavioral unit:

1. UART register, FIFO, status, and interrupt behavior for PL011 and 16550.
2. Device registration rejects duplicate consoles and exposes push/drain APIs.
3. Guest FDT generation creates the correct node, IRQ, aliases, stdout path,
   and command line while preserving init arguments.
4. Hybrid authorization permits the emulated console SPI without claiming a
   physical SPI.
5. Shell connection state recognizes `Ctrl+]`, restores normal input mode, and
   rejects stale-generation detach attempts without printing a duplicate
   prompt.
6. Excluded physical UART ranges that overlap the emulated console do not
   become reserved stage-2 ranges; unrelated excluded ranges still do.
7. Existing canonical Hybrid SPI ownership/injection and PPI 27 hardware-LR
   forwarding behavior remain covered and unchanged.

After unit tests, run `cargo fmt`, targeted `cargo xtask clippy` for the changed
crates, their relevant tests, and an AArch64 Axvisor QEMU Linux run using a
temporary VM config with an emulated console. The end-to-end check must show a
guest shell through `vm connect`, bidirectional input/output, and successful
detach back to the Axvisor shell.

Verification must also confirm that the checked-in AArch64 Linux VM config has
no diff, and run relevant non-AArch64 `axdevice`/`axvm` build or clippy checks so
the AArch64-only console integration does not alter other architecture paths.
