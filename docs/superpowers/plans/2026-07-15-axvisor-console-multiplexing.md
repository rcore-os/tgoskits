# Axvisor AArch64 VM Console Multiplexing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Selectively port AArch64 emulated UART console attachment and Axvisor shell multiplexing without cherry-picking upstream commit `5c794bcae087ba3460704ad507f9dc920bcff343` or changing the existing Hybrid/PPI 27 forwarding contract.

**Architecture:** `axdevice` owns bounded PL011/16550 register models and exposes one typed AArch64 console capability per VM. AxVM describes and authorizes the emulated console IRQ while keeping it outside physical Hybrid ownership, and the Axvisor shell uses a generation-tagged connection state to switch host input/output safely between the shell and one running VM.

**Tech Stack:** Rust 2024 nightly, `no_std` virtualization crates, `axdevice-base`, `arm_vgic`, `fdt_edit`, AxVM interrupt fabric, Axvisor `axstd` shell, QEMU AArch64 GICv3.

## Before implementation

Commit this approved plan before Task 1:

```bash
git add docs/superpowers/plans/2026-07-15-axvisor-console-multiplexing.md
git commit -m "docs(axvisor): add console multiplexing plan"
```

---

## File map

- Create `virtualization/axdevice/src/pl011.rs`: bounded PL011 register/FIFO model.
- Create `virtualization/axdevice/src/uart_16550.rs`: bounded MMIO 16550 register/FIFO model.
- Modify `virtualization/axdevice/src/lib.rs`: target/test-gated exports.
- Modify `virtualization/axdevice/src/device.rs`: one-console registration and push/drain/IRQ capability.
- Modify `virtualization/arm_vgic/src/v3/vgicd.rs`: authorize guest-controlled emulated SPIs without host routing.
- Modify `virtualization/axvm/src/arch/aarch64/vm.rs`: authorize canonical Hybrid routes plus emulated console INTID.
- Modify `virtualization/axvm/src/vm/mod.rs`: running-state checked console APIs and vCPU 0 fallback injection.
- Modify `virtualization/axvm/src/boot/fdt/core/create.rs`: PL011/16550 guest nodes and chosen/alias bootargs.
- Modify `virtualization/axvm/src/boot/fdt/core/parser.rs`: do not reserve excluded physical ranges overlapping emulated MMIO.
- Create `os/axvisor/src/shell/connection.rs`: generation-tagged connection lifecycle and console I/O routing.
- Create `os/axvisor/tests/console_connection.rs`: host-only tests that include the dependency-free connection module by path.
- Modify `os/axvisor/src/shell/mod.rs`: line editor integration and output pump.
- Modify `os/axvisor/src/shell/command/mod.rs`: connect request handoff.
- Modify `os/axvisor/src/shell/command/vm.rs`: `vm connect <VM_ID>` command.
- Do not modify `os/axvisor/configs/vms/qemu/aarch64/linux-smp1.toml`.

### Task 1: PL011 and 16550 UART cores

**Files:**
- Create: `virtualization/axdevice/src/pl011.rs`
- Create: `virtualization/axdevice/src/uart_16550.rs`
- Modify: `virtualization/axdevice/src/lib.rs`

- [ ] **Step 1: Write failing UART behavior tests**

Execute two model cycles. Cycle A adds only PL011 tests and implementation; do not create the 16550 module or tests until PL011 is committed. The PL011 model accepts Byte/Word/Dword and rejects Qword with `DeviceError::Unsupported`; its constructor rejects overflowed ranges and apertures below `0x1000`. After that commit, Cycle B applies the remaining requirements to 16550. Test PL011 FR/RIS/MIS/IMSC/ICR, peripheral IDs, TX drain, and distinguishable FIFO boundaries. Cycle B requires 16550 Byte/Word/Dword support, Qword rejection, minimum aperture `8`, LSR/IIR/IER/DLAB/DLL/DLM, TX drain, and the same distinguishable FIFO policy:

```rust
#[test]
fn pl011_rx_interrupt_tracks_fifo_and_mask() {
    let uart = EmulatedPl011::try_new(0x900_0000usize.into(), 0x1000, 33).unwrap();
    uart.handle_write((0x900_0000 + REG_IMSC).into(), AccessWidth::Dword, INT_RX as usize)
        .unwrap();
    assert!(uart.push_input(b"A"));
    assert_eq!(uart.handle_read((0x900_0000 + REG_MIS).into(), AccessWidth::Dword).unwrap(), INT_RX as usize);
    assert_eq!(uart.handle_read(0x900_0000usize.into(), AccessWidth::Byte).unwrap(), b'A' as usize);
}

#[test]
fn uart_16550_drops_oldest_byte_when_fifo_is_full() {
    let uart = EmulatedUart16550::try_new(0x900_0000usize.into(), 8, 33).unwrap();
    let mut input = [b'A'; FIFO_CAPACITY + 1];
    input[0] = b'0';
    input[FIFO_CAPACITY] = b'Z';
    assert!(!uart.push_input(&input)); // RDI is masked.
    let retained = (0..FIFO_CAPACITY)
        .map(|_| uart.handle_read(0x900_0000usize.into(), AccessWidth::Byte).unwrap() as u8)
        .collect::<Vec<_>>();
    assert_eq!(retained[0], b'A');
    assert_eq!(retained[FIFO_CAPACITY - 1], b'Z');
}
```

Repeat the distinguishable-boundary assertion for each model's TX FIFO: write `b'0'`, `FIFO_CAPACITY - 1` copies of `b'A'`, then `b'Z'`; drain all bytes and expect the first retained byte to be `b'A'` and the last `b'Z'`. This distinguishes drop-oldest from drop-newest for RX and TX.

- [ ] **Step 2: Run the tests and verify RED**

Run: `cargo test -p axdevice pl011 -- --nocapture`

Expected: FAIL because `EmulatedPl011`, `try_new`, and its register behavior do not exist.


- [ ] **Step 3: Implement the minimal UART models**

Port the register semantics from the referenced upstream files, but expose fallible constructors:

```rust
pub fn try_new(base: GuestPhysAddr, size: usize, irq_id: usize) -> DeviceManagerResult<Self> {
    if size < MIN_MMIO_SIZE || base.as_usize().checked_add(size).is_none() {
        return Err(DeviceManagerError::InvalidConfig {
            operation: "initialize AArch64 console",
            detail: alloc::format!("invalid UART MMIO range at {:#x} with size {size:#x}", base.as_usize()),
        });
    }
    Ok(Self { base, size, irq_id, state: Mutex::new(State::new()) })
}
```

Use a 4096-byte FIFO whose `push_drop_oldest` replaces the oldest byte at capacity. Keep lock scope inside FIFO/register mutation and return drained bytes to the caller before host output.

- [ ] **Step 4: Run UART tests and verify GREEN**

Run: `cargo test -p axdevice pl011 -- --nocapture`

Expected: all new tests PASS.

- [ ] **Step 5: Commit the UART cores**

```bash
git add virtualization/axdevice/src/pl011.rs virtualization/axdevice/src/lib.rs
git commit -m "feat(axdevice): add emulated PL011"
```

- [ ] **Step 6: Complete the independent 16550 RED-GREEN cycle**

Now add the 16550 DLAB, width, aperture, IRQ-mask, distinguishable RX/TX overflow tests shown above. Run `cargo test -p axdevice uart_16550 -- --nocapture` and verify RED, implement only `uart_16550.rs` plus its export, rerun the same filter GREEN, then commit:

```bash
git add virtualization/axdevice/src/uart_16550.rs virtualization/axdevice/src/lib.rs
git commit -m "feat(axdevice): add emulated 16550 UART"
```

### Task 2: Register one connectable console per VM

**Files:**
- Modify: `virtualization/axdevice/src/device.rs`
- Test: `virtualization/axdevice/src/device.rs`

- [ ] **Step 1: Write failing registration tests**

Add host-testable helpers under `#[cfg(any(test, target_arch = "aarch64"))]` and tests proving subtype selection, duplicate rejection, and IRQ exposure:

```rust
#[test]
fn rejects_a_second_connectable_console() {
    let mut devices = AxVmDevices::empty();
    devices.register_aarch64_console(&console_config("pl011", 0x900_0000, 33, &[])).unwrap();
    let error = devices
        .register_aarch64_console(&console_config("uart", 0x901_0000, 34, &[1]))
        .unwrap_err();
    assert!(matches!(error, DeviceManagerError::ResourceConflict { .. }));
}
```

Also assert that explicit subtype values other than `1` are `InvalidConfig`, while an empty list selects PL011.

- [ ] **Step 2: Run the focused test and verify RED**

Run: `cargo test -p axdevice rejects_a_second_connectable_console -- --nocapture`

Expected: FAIL because no AArch64 console capability is stored.

- [ ] **Step 3: Implement the typed console capability**

Add a private enum with `Pl011(Arc<...>)` and `Uart16550(Arc<...>)`, plus:

```rust
pub fn aarch64_console_push_input(&self, bytes: &[u8]) -> Option<Option<usize>>;
pub fn aarch64_console_drain_output(&self, out: &mut [u8]) -> Option<usize>;
pub fn aarch64_console_irq(&self) -> Option<usize>;
pub fn has_aarch64_console(&self) -> bool;
```

Register the MMIO adapter first, then store the console capability. Reject a second console before either operation so failure is transactional.

- [ ] **Step 4: Run all `axdevice` tests**

Run: `cargo test -p axdevice`

Expected: PASS with no warnings.

- [ ] **Step 5: Commit registration support**

```bash
git add virtualization/axdevice/src/device.rs
git commit -m "feat(axdevice): expose VM console capability"
```

### Task 3: Separate vGIC authorization from physical ownership

**Files:**
- Modify: `virtualization/arm_vgic/src/v3/vgicd.rs`
- Modify: `virtualization/axvm/src/arch/aarch64/vm.rs`
- Modify: `virtualization/axvm/src/vm/mod.rs`
- Test: `virtualization/arm_vgic/src/v3/vgicd.rs`
- Test: `virtualization/axvm/src/config.rs`
- Test: `virtualization/axvm/src/vm/mod.rs`

- [ ] **Step 1: Write failing authorization and console API tests**

Add a `VGicD::allow_guest_irq_in` test proving INTID 48 changes only the assigned bitmap used by `irq_access_mask_in`, leaving adjacent INTIDs masked. Extract production helper `fn hybrid_guest_intids<'a>(routes: &'a [Aarch64ForwardedIrq], console_intid: Option<u32>) -> impl Iterator<Item = u32> + 'a`; the actual `register_arch_devices` Hybrid branch must consume this iterator and call `allow_guest_irq` for each result. Keep physical setup in existing `setup_hybrid_forwarding`, whose input remains only `config.aarch64_hybrid_forwarded_irqs()`. Add a production-facing AxVM test asserting the helper union contains route guest INTIDs plus the optional console INTID, while the route slice sent to physical ownership remains canonical-only.

```rust
#[test]
fn hybrid_guest_authorization_adds_console_without_physical_ownership() {
    let routes = [Aarch64ForwardedIrq::identity(Aarch64GicSpi::new(16).unwrap())];
    assert_eq!(hybrid_guest_intids(&routes, Some(80)).collect::<Vec<_>>(), [48, 80]);
    assert_eq!(routes.iter().map(|route| route.host_intid()).collect::<Vec<_>>(), [48]);
}
```

- [ ] **Step 2: Run the focused tests and verify RED**

Run: `cargo test -p arm_vgic allow_guest_irq -- --nocapture`

Run: `cargo test -p axvm hybrid_guest_authorization_adds_console_without_physical_ownership -- --nocapture`

Expected: FAIL because guest-only authorization and the union helper do not exist.

- [ ] **Step 3: Implement authorization and VM console APIs**

Implement `VGicD::allow_guest_irq` as bitmap authorization only. In the existing AArch64 `register_arch_devices` Hybrid branch, bind `let routes = config.aarch64_hybrid_forwarded_irqs(); let console_intid = devices.aarch64_console_irq().map(|irq| irq as u32);` and call `hybrid_guest_intids(routes, console_intid)`; authorize every yielded INTID. Validate the console with `(32..1020).contains(&intid)` then construct `Aarch64GicSpi::new((intid - 32) as u32)`; return `InvalidInput` on failure. Do not feed the console to `assign_irq`, `setup_hybrid_forwarding`, `SPI_OWNERS`, or host affinity setup. Re-run existing ownership-generation and PPI 27 classifier tests to prove those paths remain unchanged.

First complete and commit the authorization-only cycle after its two focused tests pass:

```bash
git add virtualization/arm_vgic/src/v3/vgicd.rs virtualization/axvm/src/arch/aarch64/vm.rs virtualization/axvm/src/config.rs
git commit -m "feat(axvm): authorize emulated console SPIs"
```

Then add an architecture-neutral private production seam in `vm/mod.rs`: `ConsoleIoPolicy::ensure_running(status)`, `require_console(option)`, and `deliver_pending_irq(pending_irq, fabric_has_backend, pulse, inject_vcpu0)`. The AArch64-only public methods below must delegate state validation, missing-console mapping, and IRQ delivery to this seam; it is not a policy copy. File-local `#[cfg(test)]` tests named `console_io_*` pass closures that record pulse and injection calls. Required RED cases use actual statuses `Ready`, `Paused`, and `Stopped`; missing console maps to `AxVmError::NotFound`; a successful fabric pulse performs no direct injection; absent backend or pulse error calls `inject_vcpu0` exactly once with the console INTID; successful fallback returns `Ok(())`; failed fallback returns its typed mock error. Run `cargo test -p axvm console_io -- --nocapture` and verify RED before implementing the seam and methods.

Add AArch64-only VM methods:

```rust
pub fn has_connect_console(&self) -> bool;
pub fn push_console_input(&self, bytes: &[u8]) -> AxVmResult;
pub fn drain_console_output(&self, out: &mut [u8]) -> AxVmResult<usize>;
```

Both I/O methods pass `self.status()` through `ensure_running`, map a missing device result through `require_console`, and pass the pending INTID to `deliver_pending_irq`. The production injection closure builds a mask containing only vCPU 0. Return the fallback injection error if it fails. Run `cargo test -p axvm console_io -- --nocapture` GREEN before the commit.

- [ ] **Step 4: Run VGIC and AxVM tests**

Run: `cargo test -p arm_vgic -p axvm`

Expected: PASS, including existing forwarding-generation and PPI classification tests.

- [ ] **Step 5: Commit IRQ authorization and VM APIs**

```bash
git add virtualization/axvm/src/vm/mod.rs
git commit -m "feat(axvm): expose AArch64 console I/O"
```

### Task 4: Describe the console in the guest FDT

**Files:**
- Modify: `virtualization/axvm/src/boot/fdt/core/create.rs`
- Modify: `virtualization/axvm/src/boot/fdt/core/parser.rs`
- Test: `virtualization/axvm/src/boot/fdt/core/create.rs`
- Test: `virtualization/axvm/src/boot/fdt/core/parser.rs`

- [ ] **Step 1: Write failing FDT and overlap tests**

Execute two independent RED-GREEN cycles. Cycle A covers PL011 and 16550 compatibles/registers, GIC SPI cells, aliases, stdout path, bootarg rewrite with `--`, invalid INTIDs; do not add parser overlap tests until Cycle A is committed:

```rust
#[test]
fn rewrite_console_bootargs_preserves_init_arguments() {
    assert_eq!(
        rewrite_console_bootargs("root=/dev/vda console=ttyAMA0 -- -n -l /bin/sh", "pl011,mmio32,0x9000000", "ttyAMA0,115200"),
        "root=/dev/vda earlycon=pl011,mmio32,0x9000000 console=ttyAMA0,115200 -- -n -l /bin/sh"
    );
}
```

- [ ] **Step 2: Run focused tests and verify RED**

Run: `cargo test -p axvm boot::fdt -- --nocapture`

Expected: FAIL because no console node, bootarg rewrite, or invalid-INTID validation exists.

- [ ] **Step 3: Implement FDT console generation**

Use the concrete helpers `append_aarch64_emulated_console_nodes(tree, crate_config, existing_fdt)`, `existing_pl011_clock_phandle`, `patch_chosen_console`, and `rewrite_console_bootargs`, based only on upstream `virtualization/axvm/src/boot/fdt/core/create.rs`. Empty `cfg_list` creates `pl011@{base:x}` with `compatible = ["arm,pl011", "arm,primecell"]`, `reg`, `interrupts = [0, intid - 32, 4]`, two identical clock phandles, `clock-names = ["uartclk", "apb_pclk"]`, 24 MHz and `status = "okay"`. `cfg_list == [1]` creates `serial@{base:x}` with `compatible = "ns16550a"`, `reg`, the same interrupt cells, 1,843,200 Hz, 115,200 current speed, `reg-shift = 0`, `reg-io-width = 1`, and `status = "okay"`. Reject any other subtype before mutating the tree. Validate `(32..1020).contains(&intid)` and encode the SPI offset only after that check. Reuse `/apb-pclk` phandle when present; otherwise allocate a collision-free phandle by scanning existing `phandle` and `linux,phandle` values before adding a 24 MHz fixed-clock node. Set `/aliases/serial0` and `/chosen/stdout-path` to the first console. Rewrite only tokens before the first standalone `--`, removing old `console=` and `earlycon=`, then append the model-specific values; preserve `--` and every init argument verbatim.

Run the full `boot::fdt` scope GREEN, then commit Cycle A with `git add virtualization/axvm/src/boot/fdt/core/create.rs && git commit -m "feat(axvm): describe attached AArch64 consoles"`.

Cycle B adds `excluded_uart_overlap_does_not_reserve_emulated_console_range`, `excluded_uart_partial_overlap_retains_prefix_and_suffix`, and `excluded_uart_unrelated_range_remains_reserved`. The partial case reserves `[0x8fff_f000, 0x9000_2000)` around emulated `[0x9000_0000, 0x9000_1000)` and expects exactly the prefix plus suffix; the unrelated case expects `[0xa000_0000, 0xa000_1000)` unchanged. Run `cargo test -p axvm excluded_uart -- --nocapture` and verify all three fail for missing subtraction. Then add an interval-subtraction helper returning zero, one, or two `ReservedAddressConfig` pieces. Apply it only against emulated Console ranges, then feed each remainder through existing merge logic. Run the same filter GREEN and commit with `git add virtualization/axvm/src/boot/fdt/core/parser.rs && git commit -m "fix(axvm): preserve non-console excluded MMIO"`.

- [ ] **Step 4: Run FDT and AxVM tests**

Run: `cargo test -p axvm boot::fdt -- --nocapture`

Expected: PASS, including existing canonical Hybrid host-FDT route tests.

### Task 5: Add generation-safe shell multiplexing

**Files:**
- Create: `os/axvisor/src/shell/connection.rs`
- Create: `os/axvisor/tests/console_connection.rs`
- Modify: `os/axvisor/src/shell/mod.rs`
- Modify: `os/axvisor/src/shell/command/mod.rs`
- Modify: `os/axvisor/src/shell/command/vm.rs`

- [ ] **Step 1: Write failing host tests for the pure state machine**

Keep `connection.rs` limited to `core` atomics and plain byte slices. In `tests/console_connection.rs`, include it with `#[path = "../src/shell/connection.rs"] mod connection;`. Test these production APIs:

```rust
let state = ConsoleConnectionState::new();
let old = state.connect(1).unwrap();
assert!(state.detach(old).is_some());
let new = state.connect(1).unwrap();
assert!(state.detach(old).is_none());
assert_eq!(state.current(), Some(new));

assert_eq!(split_console_input(b"abc\x1dignored"), (b"abc".as_slice(), true));
assert_eq!(split_console_input(b"abc"), (b"abc".as_slice(), false));
assert_eq!(split_console_input(b"\x1d"), (b"".as_slice(), true));
```

Also test that a second `connect` returns `AlreadyConnected`; `detach(snapshot)` succeeds once; a stale output-pump snapshot cannot detach a reconnection; and a detach result exposes one `Detached { vm_id }` event so only its caller may print a return message and prompt.

- [ ] **Step 2: Run the host test and verify RED**

Run: `cargo test -p axvisor --test console_connection -- --nocapture`

Expected: FAIL because `connection.rs`, tokens, split logic, and exact-token detachment do not exist. This command intentionally selects only the host integration test, avoiding the bare-metal Axvisor binary dependency set.

- [ ] **Step 3: Implement the dependency-free connection state**

Define `ConnectionToken { vm_id, generation }`, `ConnectError::AlreadyConnected`, and `DetachEvent::Detached { vm_id }`. Use a narrow `spin::Mutex` only if that dependency remains available to the host integration test; otherwise use one packed `AtomicU64` with `0` as disconnected, low 32 bits as `vm_id + 1`, and high 32 bits as a nonzero wrapping generation. `connect` must CAS disconnected to a fresh token. `current` returns a snapshot token. `detach(token)` CASes exactly that token to disconnected and returns the event only on success. `split_console_input` returns bytes before the first `0x1d` and discards that byte plus the remaining bytes in the same read.

- [ ] **Step 4: Write the shell ownership flow before integrating I/O**

After the pure tests pass, commit `connection.rs` and its integration test as `feat(axvisor): add console connection state`.

Refactor the current local editor variables into the concrete `ShellLineEditor { history: CommandHistory, buf: [u8; MAX_LINE_LEN], cursor: usize, line_len: usize, input_state: InputState }`. Move the current CR/LF submission, backspace, escape-sequence, history, redraw, and ordinary-character branches unchanged into `new`, `submit_current_line`, `handle_normal_byte`, `handle_escape_sequence`, and `handle_byte`; add only `reset_input_state`. This is a behavior-preserving extraction before multiplexing. The shell module owns `static CONNECTION: ConsoleConnectionState`; `command/mod.rs` alone owns `static CONNECT_REQUEST: AtomicUsize` and its publish/take helpers.

`vm connect` validates one numeric ID, VM existence, `VmStatus::Running`, AArch64 target, and `has_connect_console()`, then publishes with `compare_exchange(NO_CONNECT_REQUEST, vm_id, ...)`; it never mutates `CONNECTION` or prints the attached banner.

`ShellLineEditor::submit_current_line` executes the command, clears the editor, then atomically takes the pending request. If absent, it prints the ordinary prompt. If present, it calls `CONNECTION.connect(vm_id)`; on success it prints exactly one attached banner and suppresses the ordinary prompt, while on failure it prints the error and one ordinary prompt. This is the sole prompt-suppression decision after a command.

Start exactly one output-pump thread from `console_init` before the initial prompt. It runs for the shell lifetime, sleeps 10 ms while disconnected, and for each iteration:
1. snapshots `ConnectionToken`;
2. looks up the VM and verifies `Running`;
3. drains repeatedly into `[u8; 256]` until zero, writing and flushing only after `drain_console_output` releases device locks;
4. rechecks `CONNECTION.current() == Some(snapshot)` before every drain and before handling an error;
5. on missing/stopped VM or drain error, calls `detach(snapshot)`; only the winner that receives `Detached` calls `print_detached(vm_id)`, which alone prints the return message and prompt.

The blocking stdin loop remains valid because the independent output-pump thread owns asynchronous guest output. For connected input it snapshots the token, splits on Ctrl+], forwards only the prefix after rechecking the same snapshot and VM `Running`, then detaches on Ctrl+] or on missing/stopped VM/push error. Again, only the successful exact-token detach calls `print_detached`. Reset the line-editor escape state after that successful detach. A concurrent pump or input failure therefore cannot print a duplicate prompt or detach a newer connection.

After implementing only the editor extraction and pending-request handoff, run the AArch64 build, then commit `shell/mod.rs`, `command/mod.rs`, and `command/vm.rs` as `feat(axvisor): add VM console attach command`. Do not add the output pump or connected input in that commit.

- [ ] **Step 5: Implement connected input and output pumping**

Put pending-request helpers in `command/mod.rs`; add `vm connect <VM_ID>` and help text in `command/vm.rs`; integrate the state flow above in `shell/mod.rs`. Non-AArch64 builds keep the command but return the explicit unsupported message and never reference AArch64-only VM APIs.

- [ ] **Step 6: Verify state tests and AArch64 build**

Run: `cargo test -p axvisor --test console_connection -- --nocapture`

Run: `cargo xtask axvisor build --config test-suit/axvisor/normal/qemu/build-aarch64-unknown-none-softfloat.toml --arch aarch64`

Expected: tests and build PASS with exactly one output-pump creation site.

- [ ] **Step 7: Commit shell multiplexing**

```bash
git add os/axvisor/src/shell/mod.rs
git commit -m "feat(axvisor): pump attached VM console I/O"
```

### Task 6: Full regression and AArch64 QEMU validation

**Files:**
- Temporary only: `/tmp/axvisor-linux-aarch64-console.toml`
- Temporary only: `/tmp/qemu-aarch64-console.toml`

- [ ] **Step 1: Verify the checked-in VM config is unchanged**

Run: `git diff --exit-code a13f2ceee -- os/axvisor/configs/vms/qemu/aarch64/linux-smp1.toml`

Expected: no output and exit 0.

- [ ] **Step 2: Run formatting and whitespace validation**

Run: `cargo fmt --all -- --check`

Run: `git diff --check a13f2ceee`

Expected: both exit 0.

- [ ] **Step 3: Run affected tests and clippy**

Run: `cargo test -p axdevice -p arm_vgic -p axvm -p axvm-types -p axvmconfig`

Run: `cargo test -p axvm console_io -- --nocapture`

Run: `cargo xtask clippy --package axdevice`

Run: `cargo xtask clippy --package arm_vgic`

Run: `cargo xtask clippy --package axvm`

Expected: all tests and every clippy matrix entry PASS.

- [ ] **Step 4: Check non-AArch64 paths**

Run:

```bash
cargo xtask axvisor build --config test-suit/axvisor/normal/qemu/build-riscv64gc-unknown-none-elf.toml --arch riscv64
cargo xtask axvisor build --config test-suit/axvisor/normal/qemu/build-loongarch64-unknown-none-softfloat.toml --arch loongarch64
cargo xtask axvisor build --config test-suit/axvisor/normal/qemu/build-x86_64-unknown-none-vmx.toml --arch x86_64
```

Expected: all three builds PASS; target gating adds no UART or shell API reference on non-AArch64 builds.

- [ ] **Step 5: Run AArch64 Hybrid Linux with reproducible temporary configs**

Copy the checked-in files only to `/tmp`:

```bash
cp os/axvisor/configs/vms/qemu/aarch64/linux-smp1.toml /tmp/axvisor-linux-aarch64-console.toml
cp test-suit/axvisor/normal/qemu/smoke/qemu-aarch64.toml /tmp/qemu-aarch64-console.toml
```

Edit only `/tmp/axvisor-linux-aarch64-console.toml` to this exact delta:

```diff
-phys_cpu_ids = [0]
+phys_cpu_ids = [1]
+phys_cpu_sets = [2]
@@
 kernel_load_addr = 0x8020_0000
+cmdline = "root=/dev/vda rw init=/sbin/getty -- -n -l /bin/sh -L 115200 ttyAMA0"
@@
-interrupt_mode = "passthrough"
+interrupt_mode = "hybrid"
@@
 excluded_devices = [
+   ["/pl011@9000000"],
+   ["/flash@0"],
 ]
@@
 emu_devices = [
+  ["pl011-console", 0x0900_0000, 0x1000, 33, 0x2, []],
+  ["gppt-gicd", 0x0800_0000, 0x1_0000, 0, 0x21, []],
+  ["gppt-gicr", 0x080a_0000, 0x2_0000, 0, 0x20, [1, 0x2_0000, 1]],
 ]
```

The temporary QEMU file is an exact copy so it keeps GICv3, four pCPUs, the Alpine disk, 8 GiB RAM, and `-nographic`. Run:

```bash
cargo xtask axvisor qemu --config test-suit/axvisor/normal/qemu/build-aarch64-unknown-none-softfloat.toml --arch aarch64 --vmconfigs /tmp/axvisor-linux-aarch64-console.toml --qemu-config /tmp/qemu-aarch64-console.toml --rootfs tmp/axbuild/rootfs/rootfs-aarch64-alpine.img
```

At the Axvisor prompt enter `vm connect 1`. Wait for the Linux `ttyAMA0` shell, enter `echo console-mux-ok`, verify that exact text, then send byte `0x1d` with Ctrl+]. Verify one return banner and one Axvisor prompt. Search captured output for `Unhandled IRQ HwIrq(27)`, `ownership conflict`, and `SPI owner`; none may indicate an error. Exit QEMU with Ctrl+A then X.

Expected: Linux boots in Hybrid mode through the emulated PL011; host PL011 is excluded; vCPU 0 of the VM is pCPU 1; no PPI 27 storm or physical SPI ownership conflict occurs.

- [ ] **Step 6: Record final status**

Run: `git status --short`

Expected: clean worktree; temporary files remain only under `/tmp`.
