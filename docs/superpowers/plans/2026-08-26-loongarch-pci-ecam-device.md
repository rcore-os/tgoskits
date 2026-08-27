# LoongArch Guest PCI ECAM Device Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a VM-owned LoongArch PCI ECAM configuration device whose resolved MMIO resource is also the authoritative ACPI MCFG/PCI0 aperture, allowing Linux PCI probing to complete without exposing host PCI resources.

**Architecture:** Follow the existing x86 PCI-config pattern: an AxDevice runtime object implements configuration-space access, while a LoongArch AxVM `DeviceModel` owns both the fixed ECAM resource and its ACPI `PciHostBridge` contribution. LoongArch UEFI selects strict ACPI plus partial auxiliary FDT resolution; direct-FDT guests retain strict FDT behavior and do not register the ACPI-only ECAM model.

**Tech Stack:** Rust 2024 `no_std`, AxDevice, AxVM resolved device graph, LoongArch nested paging, ACPI MCFG/AML, QEMU-LVZ, Linux guest.

**Execution constraint:** The user requested no commits. This explicit instruction overrides the
execution skills' normal worktree, per-task commit, branch-finishing, merge, and push steps. Use the
required execution skill only for task sequencing and two-stage review. Do not run `git add`,
`git commit`, `git reset`, rebase, branch cleanup, merge, or push. Preserve the existing staged
virtio-blk diff; all implementation changes remain unstaged and are reviewed with `git diff`, while
the pre-existing staged work remains visible through `git diff --cached`. Record
`git diff --cached --binary | sha256sum` before Task 1 and after Task 5; the hashes must match.

---

### Task 1: Implement the architecture-neutral PCI ECAM runtime device

**Files:**
- Create: `virtualization/axdevice/src/pci/mod.rs`
- Create: `virtualization/axdevice/src/pci/ecam.rs`
- Modify: `virtualization/axdevice/src/lib.rs`
- Test: `virtualization/axdevice/src/pci/ecam.rs`

- [x] **Step 1: Add failing constructor and absent-BDF tests**

Add unit tests using a real `DeviceAccess` and test `DeviceContext` for:

```rust
assert!(PciEcamDevice::new(0x2000_0000, 0).is_err());
assert!(PciEcamDevice::new(0x2000_1000, 0x0800_0000).is_err());
assert!(PciEcamDevice::new(0x2000_0000, 0x0800_0001).is_err());
assert!(PciEcamDevice::new(0x2000_0000, 0x1010_0000).is_err());

assert_eq!(read_byte(&device, 0x2000_0000), 0xff);
assert_eq!(read_word(&device, 0x2000_0000), 0xffff);
assert_eq!(read_dword(&device, 0x2000_0000), 0xffff_ffff);
```

Also test base-relative decoding at a nonzero bus/device/function address, ignored writes, natural-alignment rejection, Qword rejection, aperture-end rejection, and accesses crossing a 4-KiB function boundary.

- [x] **Step 2: Run the focused test and verify red**

Run:

```bash
cargo test -p axdevice pci::ecam::tests -- --nocapture
```

Expected: compilation fails because `PciEcamDevice` and the `pci` module do not exist.

- [x] **Step 3: Implement the minimal ECAM device**

Implement:

```rust
pub struct PciEcamDevice {
    base: u64,
    size: u64,
    resources: Box<[Resource]>,
}
```

Constructor invariants:

- base aligned to `1 << 20`;
- size nonzero, 1-MiB granular, and at most `256 << 20`;
- `base + size` does not overflow.

Access invariants:

- bus = `offset >> 20`;
- device = `(offset >> 15) & 0x1f`;
- function = `(offset >> 12) & 0x7`;
- register = `offset & 0xfff`;
- widths Byte/Word/Dword only and naturally aligned;
- `register + width <= 0x1000`;
- every BDF is initially absent, so reads return width-matched all ones and writes succeed without state change.

Use typed `DeviceError::{InvalidInput, InvalidWidth, OutOfRange, Unsupported}`; do not allocate ECAM backing memory or add endpoint-registration abstractions.

- [x] **Step 4: Run focused tests and clippy**

Run:

```bash
cargo test -p axdevice pci::ecam::tests -- --nocapture
cargo xtask clippy --package axdevice
```

Expected: ECAM tests pass and both AxDevice clippy profiles pass.

### Task 2: Add strict and auxiliary firmware-interface resolution

**Files:**
- Modify: `virtualization/axvm/src/boot/fdt/device.rs`
- Test: `virtualization/axvm/src/boot/fdt/device.rs`

- [x] **Step 1: Add a failing mixed-interface test**

Build a resolved graph containing one FDT-capable model and one ACPI-only model. Assert:

- existing strict `resolve_fdt_firmware()` rejects the graph;
- new `resolve_available_fdt_firmware()` returns only the real FDT contributions;
- strict ACPI resolution still requires every platform-described UEFI model to support ACPI.

- [x] **Step 2: Run the mixed-interface test and verify red**

Run:

```bash
cargo test --locked -p axvm --features host-test --no-default-features --lib \
  boot::fdt::device::tests -- --nocapture
```

Expected: compilation fails because `resolve_available_fdt_firmware()` does not exist.

- [x] **Step 3: Extract one resolver core and preserve strict behavior**

Keep `resolve_fdt_firmware()` unchanged externally: it calls `validate_fdt_support()` before the shared resolution core. Add a `pub(crate)` auxiliary resolver that skips validation and resolves only nodes with actual FDT contributions. Its name and rustdoc must state that it is for a secondary firmware artifact after another interface was selected strictly; it must not become a silent fallback for direct-FDT guests.

- [x] **Step 4: Run focused tests and AxVM clippy**

Run:

```bash
cargo test --locked -p axvm --features host-test --no-default-features --lib \
  boot::fdt::device::tests -- --nocapture
cargo xtask clippy --package axvm
```

Expected: mixed-interface tests pass; all AxVM clippy profiles pass.

### Task 3: Add the LoongArch PCI ECAM device model and machine-plan node

**Files:**
- Create: `virtualization/axvm/src/arch/loongarch64/pci_ecam.rs`
- Modify: `virtualization/axvm/src/arch/loongarch64/mod.rs`
- Modify: `virtualization/axvm/src/arch/loongarch64/vm.rs`
- Modify: `virtualization/axvm/src/arch/loongarch64/boot/probe.rs`
- Test: `virtualization/axvm/src/arch/loongarch64/pci_ecam.rs`
- Test: `virtualization/test_crates/virtualization-tests/tests/configured_device_graph.rs`

- [x] **Step 1: Add failing resource/model tests**

Test the model contract through real device-graph APIs:

- UEFI plan contains one `pci-ecam` node;
- direct-FDT/non-UEFI plan does not contain the ACPI-only node;
- the node requests one fixed MMIO range matching the normalized host-or-default ECAM profile;
- overlap with guest RAM or another fixed resource fails planning;
- the model contributes one ACPI `PciHostBridge` and builds one `PciEcamDevice`.

- [x] **Step 2: Run lowest available tests/checks and verify red**

Run common host graph tests first as regression coverage:

```bash
cargo test --locked -p virtualization-tests --test configured_device_graph -- --nocapture
```

Run the real LoongArch model tests inside the LVZ container with its musl linker and user-mode
runner:

```bash
docker run --rm -v "$PWD:/workspace" -w /workspace \
  ghcr.io/rcore-os/tgoskits-container-axvisor-lvz:latest \
  bash -lc 'rustup target add loongarch64-unknown-linux-musl >/dev/null && \
    CARGO_TARGET_LOONGARCH64_UNKNOWN_LINUX_MUSL_LINKER=/opt/loongarch64-linux-musl-cross/bin/loongarch64-linux-musl-gcc \
    CARGO_TARGET_LOONGARCH64_UNKNOWN_LINUX_MUSL_RUNNER=/usr/bin/qemu-loongarch64-static \
    RUSTFLAGS="-C target-feature=+crt-static" \
    cargo test --locked -p axvm --target loongarch64-unknown-linux-musl \
      --features host-test --no-default-features --lib \
      arch::loongarch64::pci_ecam::tests -- --nocapture'
```

Expected: the common graph suite remains green, while the LoongArch test compile/run fails because
the model is absent.

- [x] **Step 3: Implement the LoongArch model**

Create `LoongArchPciEcamModel` holding the normalized `PciHost` profile. It must:

- request ECAM through `ResourceRequest::Fixed`;
- contribute ACPI-only `PciHostBridge(AcpiDeviceSpec::new("PCI0", "PNP0A08"))` with the ECAM register slot;
- build `PciEcamDevice` from the resolved resource;
- verify the resolved resource equals its normalized profile.

Refactor `boot::probe` to expose one fallible normalized PCI-profile helper used by VM planning and guest-platform construction. For UEFI configurations, add the model node in `plan_devices()`; do not add it for direct-FDT configurations. Keep fixed values in the machine/profile layer rather than AxDevice.

- [x] **Step 4: Run graph tests, target check, and targeted clippy**

Run:

```bash
cargo test --locked -p virtualization-tests --test configured_device_graph -- --nocapture
docker run --rm -v "$PWD:/workspace" -w /workspace \
  ghcr.io/rcore-os/tgoskits-container-axvisor-lvz:latest \
  bash -lc 'rustup target add loongarch64-unknown-linux-musl >/dev/null && \
    CARGO_TARGET_LOONGARCH64_UNKNOWN_LINUX_MUSL_LINKER=/opt/loongarch64-linux-musl-cross/bin/loongarch64-linux-musl-gcc \
    CARGO_TARGET_LOONGARCH64_UNKNOWN_LINUX_MUSL_RUNNER=/usr/bin/qemu-loongarch64-static \
    RUSTFLAGS="-C target-feature=+crt-static" \
    cargo test --locked -p axvm --target loongarch64-unknown-linux-musl \
      --features host-test --no-default-features --lib \
      arch::loongarch64::pci_ecam::tests -- --nocapture'
```

Expected: graph tests and target checks pass; no direct-FDT regression.

### Task 4: Make LoongArch ACPI consume the resolved ECAM resource

**Files:**
- Modify: `virtualization/axvm/src/arch/loongarch64/boot/mod.rs`
- Modify: `virtualization/axvm/src/arch/loongarch64/boot/probe.rs`
- Modify: `virtualization/axvm/src/arch/loongarch64/boot/acpi/config.rs`
- Modify: `virtualization/axvm/src/arch/loongarch64/boot/acpi/composer.rs`
- Modify: `virtualization/axvm/src/arch/loongarch64/boot/acpi/aml.rs`
- Modify: `virtualization/axvm/src/arch/loongarch64/boot/acpi/tables.rs`
- Test: corresponding `#[cfg(test)]` modules in those files

- [x] **Step 1: Add failing ACPI ownership tests**

Add deterministic tests for:

- UEFI resolution accepts three shared FDT specials and four ACPI specials including PCI;
- direct-FDT resolution remains strict and unchanged;
- exactly one PCI special named `PCI0` with HID `PNP0A08` and one MMIO register is required;
- host/profile ECAM mismatch fails with both ranges in the error;
- MCFG base is the resolved graph base;
- MCFG start bus is zero and end bus is `(size >> 20) - 1`;
- PCI0 `_CRS` uses the same bus range;
- malformed, missing, and duplicate PCI contributions fail.

- [x] **Step 2: Run focused/target checks and verify red**

Run available common ACPI tests plus the actual LoongArch ACPI tests in the LVZ container:

```bash
cargo test --locked -p axvm --features host-test --no-default-features --lib \
  boot::acpi::device::tests -- --nocapture
docker run --rm -v "$PWD:/workspace" -w /workspace \
  ghcr.io/rcore-os/tgoskits-container-axvisor-lvz:latest \
  bash -lc 'rustup target add loongarch64-unknown-linux-musl >/dev/null && \
    CARGO_TARGET_LOONGARCH64_UNKNOWN_LINUX_MUSL_LINKER=/opt/loongarch64-linux-musl-cross/bin/loongarch64-linux-musl-gcc \
    CARGO_TARGET_LOONGARCH64_UNKNOWN_LINUX_MUSL_RUNNER=/usr/bin/qemu-loongarch64-static \
    RUSTFLAGS="-C target-feature=+crt-static" \
    cargo test --locked -p axvm --target loongarch64-unknown-linux-musl \
      --features host-test --no-default-features --lib \
      arch::loongarch64::boot::tests -- --nocapture'
```

Expected: the new LoongArch ownership tests fail because ACPI still reads an independent ECAM copy.

- [x] **Step 3: Select firmware interfaces explicitly**

In `GuestPlatform::discover()`:

- UEFI: call strict ACPI resolution and auxiliary available-FDT resolution;
- direct FDT: call strict FDT resolution and do not require ACPI-only PCI;
- accept the expected asymmetric special counts without weakening generic resolvers;
- resolve and validate one PCI special from ACPI for UEFI.

- [x] **Step 4: Normalize and validate ECAM ownership**

After host resources are applied, compare `GuestPlatform.pci.ecam` with the resolved PCI special.
Reject a mismatch with both ranges in the diagnostic. Assign the resolved graph range as the final
`GuestPlatform.pci.ecam`; derive bus end from its size. Remove any independent default/host ECAM
selection from the ACPI composer path.

- [x] **Step 5: Run focused tests and LoongArch checks**

Run:

```bash
cargo test --locked -p axvm --features host-test --no-default-features --lib \
  boot::acpi::device::tests -- --nocapture
docker run --rm -v "$PWD:/workspace" -w /workspace \
  ghcr.io/rcore-os/tgoskits-container-axvisor-lvz:latest \
  bash -lc 'rustup target add loongarch64-unknown-linux-musl >/dev/null && \
    CARGO_TARGET_LOONGARCH64_UNKNOWN_LINUX_MUSL_LINKER=/opt/loongarch64-linux-musl-cross/bin/loongarch64-linux-musl-gcc \
    CARGO_TARGET_LOONGARCH64_UNKNOWN_LINUX_MUSL_RUNNER=/usr/bin/qemu-loongarch64-static \
    RUSTFLAGS="-C target-feature=+crt-static" \
    cargo test --locked -p axvm --target loongarch64-unknown-linux-musl \
      --features host-test --no-default-features --lib \
      arch::loongarch64::boot::tests -- --nocapture'
```

Expected: ownership tests, target compilation, and clippy pass.

### Task 5: Synchronize documentation and run end-to-end validation

**Files:**
- Modify: `.claude/skills/arch-platform-porting/SKILL.md`
- Keep uncommitted: `docs/superpowers/specs/2026-08-26-loongarch-pci-ecam-device-design.md`
- Keep uncommitted: `docs/superpowers/plans/2026-08-26-loongarch-pci-ecam-device.md`

- [x] **Step 1: Update the architecture contract**

Document that LoongArch UEFI PCI0/MCFG is backed by a graph-owned ECAM runtime device, that host
ECAM is never identity-mapped, and that primary ACPI plus auxiliary FDT selection is explicit.

- [ ] **Step 2: Format and run the targeted validation ladder**

Run:

```bash
cargo fmt --all
cargo fmt --all --check
cargo test -p axdevice pci::ecam::tests -- --nocapture
cargo test --locked -p virtualization-tests --test configured_device_graph -- --nocapture
cargo xtask clippy --package axdevice
cargo xtask clippy --package axvm
cargo xtask clippy --package loongarch_vcpu
docker run --rm -v "$PWD:/workspace" -w /workspace \
  ghcr.io/rcore-os/tgoskits-container-axvisor-lvz:latest \
  bash -lc 'rustup target add loongarch64-unknown-linux-musl >/dev/null && \
    CARGO_TARGET_LOONGARCH64_UNKNOWN_LINUX_MUSL_LINKER=/opt/loongarch64-linux-musl-cross/bin/loongarch64-linux-musl-gcc \
    RUSTFLAGS="-C target-feature=+crt-static" \
    cargo clippy --locked -p axvm --target loongarch64-unknown-linux-musl \
      --features host-test --no-default-features --lib -- -D warnings'
```

Expected: all host and LoongArch-target checks pass without warning suppression. The direct
LoongArch clippy command is the mandatory evidence for architecture-gated AxVM code; host xtask
clippy alone is not sufficient.

**Baseline-blocked:** the full command above reached an existing `ax-task` target lint and failed
with `clippy::needless_return` at `os/arceos/modules/axtask/src/run_queue.rs:188`. The focused
modified-crate command

```bash
cargo clippy --locked --no-deps -p axvm --target loongarch64-unknown-linux-musl \
  --features host-test --no-default-features --lib -- -D warnings
```

also failed on existing target-specific dead-code diagnostics and
`clippy::single_range_in_vec_init` at the unchanged
`virtualization/axvm/src/arch/loongarch64/vm.rs:125` line. No line added or modified by the ECAM or
firmware changes produced a warning, but the baseline prevents marking this validation step as
passed.

- [x] **Step 3: Build and run the exact QEMU-LVZ mount regression**

Run without modifying tracked config files:

```bash
docker run --rm \
  -v "$PWD:/workspace" \
  -v "$PWD/tmp/virtio-blk-linux-mount/ostool/ovmf:/tmp/ostool/ovmf" \
  -w /workspace \
  -e TGOS_OVMF_DIR=/tmp/ostool/ovmf \
  ghcr.io/rcore-os/tgoskits-container-axvisor-lvz:latest \
  bash -lc 'cargo xtask axvisor qemu \
    --config tmp/virtio-blk-linux-mount/build-loongarch64.toml \
    --qemu-config tmp/virtio-blk-linux-mount/qemu-loongarch64-shell.toml'
```

Expected:

```text
Hardware virtualization support enabled on core 0
VM[1] boot success
virtio_blk ... [vda]
VIRTIO_BLK_MOUNT_PASS
=== SUCCESS PATTERN MATCHED ===
```

Forbidden output:

```text
unhandled nested page fault
hardware virtualization is not supported
VIRTIO_BLK_MOUNT_FAIL
```

- [x] **Step 4: Run cross-architecture regression checks**

Run the existing AArch64 mount command and supported x86/RISC-V checks to confirm common device-graph behavior remains intact. Record any environment-only `/dev/kvm` limitation explicitly rather than weakening tests.

AArch64 and RISC-V reached the Linux `virtio_blk` device, `VIRTIO_BLK_MOUNT_PASS`, and the success
regex. The x86 prerequisite check was blocked because `/dev/kvm` is absent in the current host, so
the x86 QEMU command was not run and is not claimed as passing.

- [x] **Step 5: Verify workspace integrity without staging or committing**

Run:

```bash
git diff --check
git diff --cached --check
git diff --cached --binary | sha256sum
git diff --name-only --diff-filter=U
git status --short --branch
```

Expected: no whitespace errors, conflicts, or unintended tracked files; existing staged virtio-blk changes remain staged and all ECAM implementation/design/plan changes remain unstaged.
