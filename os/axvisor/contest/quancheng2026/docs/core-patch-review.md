# Core Patch Review Notes

This note separates the current AxVisor core work from the first-stage contest
artifact directory. The goal is to make the final PR reviewable: contest
sources and evidence can land first, while the core scheduler, interrupt,
timer, and tooling changes can be reviewed as smaller follow-up patches.

## Current Upstream Status

As of the current `upstream/dev` head used by PR #1703, the functional AArch64
physical-timer, GIC EOI-mode and explicit passthrough IRQ work is already present
upstream through the AxVisor core history, including PR #1770. The patch notes
below are kept as traceability for the contest evidence and for reviewers who
want to connect the measured dual-guest behavior back to the core changes that
made it reliable.

## Remaining Patch Guidance

1. Commit `os/axvisor/contest/quancheng2026/` only.
2. Treat VM config, vTimer and GIC EOI-mode entries below as already-landed
   traceability unless a reviewer asks for additional split-out evidence.
3. Keep bounded diagnostics optional; include them only if reviewer evidence or
   later experiments still need them.
4. Keep `axbuild image` and `quick-start.sh` helper changes separate from the
   contest result path.

## Patch Candidate 1: VM Config And vTimer

Patch bundle:

```text
core-01-vmconfig-vtimer.patch
SHA256 52f4909c41c316bacdb25d57bcf0771ad66ef8995a0492bac36f37bbcc847ff8
```

Candidate files:

```text
os/axvisor/src/config.rs
virtualization/axvm/src/config.rs
virtualization/axvmconfig/src/lib.rs
virtualization/axvmconfig/src/templates.rs
virtualization/axvmconfig/src/test.rs
virtualization/arm_vgic/src/vtimer/cntp_cval_el0.rs
virtualization/arm_vgic/src/vtimer/cntp_timer.rs
virtualization/arm_vgic/src/vtimer/cntp_ctl_el0.rs
virtualization/arm_vgic/src/vtimer/cntp_tval_el0.rs
virtualization/arm_vgic/src/vtimer/mod.rs
virtualization/axdevice/src/adapter.rs
```

Purpose:

- Adds explicit `passthrough_irqs` propagation from AxVisor VM config into
  runtime VM config, with de-duplication.
- Adds guest-visible `CNTP_CVAL_EL0`.
- Makes `CNTP_CTL_EL0`, `CNTP_TVAL_EL0`, and `CNTP_CVAL_EL0` share one timer
  state.
- Rearms the host timer and injects virtual physical timer PPI 30 when the
  guest physical timer expires.

Why it matters for the contest:

- The Zephyr/e1000 RTOS guest depends on deterministic timer behavior for
  native RTOS scheduling and periodic probes.
- Explicit IRQ passthrough makes interrupt routing auditable instead of hiding
  it inside ad-hoc experiment configuration.

Observed validation:

```text
cargo test -p axvmconfig -p axvm -p arm_vgic --lib
```

passed with `axvmconfig 18/18`, `axvm 110/110`, and `arm_vgic 5/5`.

## Patch Candidate 2: GIC EOI Mode

Patch bundle:

```text
core-02-gic-eoi-mode.patch
SHA256 9bcc107c630541a2753d6e300da0edc542e19baf8d8ec75cf911bbe7c61c0d01
```

Candidate file:

```text
platforms/somehal/src/arch/aarch64/gic/v3.rs
```

Purpose:

- Changes the GICv3 CPU interface under `hv` from two-step EOI mode to the
  architectural reset-style mode where EOIR drops priority and deactivates the
  interrupt.

Why it matters for the contest:

- The Zephyr/e1000 and Linux/RTOS dual-guest path was sensitive to interrupt
  completion behavior. This change should be reviewed as an interrupt-path
  fix, not mixed with diagnostic logs.

Risk:

- This affects all `hv` GICv3 users. It should be validated on the same QEMU
  AArch64 dual-guest scenario and, if possible, at least one existing non-contest
  AxVisor boot path.


Observed validation:

```text
rustup target add aarch64-unknown-none-softfloat
cargo fmt --check -p somehal
CARGO_BUILD_JOBS=1 cargo check -p somehal --features hv --target aarch64-unknown-none-softfloat
```

The check passed in a clean temporary worktree after applying only this patch. The formatted patch SHA256 is `9bcc107c630541a2753d6e300da0edc542e19baf8d8ec75cf911bbe7c61c0d01`.

## Patch Candidate 3: Bounded Diagnostics

Patch bundle:

```text
core-03-bounded-diagnostics.patch
SHA256 3e3993ebf1a869517afd8044741c47726f07a2694eb15e8457b7d23e1bf60eb7
```

Candidate files:

```text
virtualization/arm_vcpu/src/context_frame.rs
virtualization/arm_vcpu/src/exception.rs
virtualization/arm_vcpu/src/vcpu.rs
virtualization/arm_vgic/src/v3/vgicd.rs
```

Purpose:

- Adds bounded `debug!` traces for synchronous exceptions, EL2 IRQ exits, guest
  register snapshots, and vGICD accesses.

Why it matters for the contest:

- These traces helped isolate the Zephyr e1000 and dual-guest interrupt path.
  They are useful for reproducibility and debugging, but they are not essential
  to the final demo path after the evidence has been collected.

Recommendation:

- Keep this patch out of the main functional PR unless reviewers ask for the
  extra diagnosis hooks.
- If included, keep it at `debug!` level and bounded by sample limits.

Observed validation:

```text
git apply --check core-03-bounded-diagnostics.patch
cargo fmt --check -p arm_vcpu -p arm_vgic
CARGO_BUILD_JOBS=1 cargo test -p arm_vcpu --lib
CARGO_BUILD_JOBS=1 cargo test -p arm_vgic --lib
CARGO_BUILD_JOBS=1 cargo check -p arm_vcpu --target aarch64-unknown-none-softfloat
CARGO_BUILD_JOBS=1 cargo check -p arm_vgic --target aarch64-unknown-none-softfloat
```

The checks passed in a clean temporary worktree after applying only this patch.
The host unit-test crates currently report `0` tests for this patch alone, so
the aarch64 compile checks are the main syntax/type coverage for the changed ARM
paths.

## Patch Candidate 4: Axbuild Image Helper

Patch bundle:

```text
core-04-axbuild-image-helper.patch
SHA256 117c1cb719ddc60e7943ce48c6bb630e54d60d7db55c30c961bd59d2decd4655
```

Candidate files:

```text
scripts/axbuild/src/axvisor/mod.rs
os/axvisor/scripts/quick-start.sh
```

Purpose:

- Wires `cargo axvisor image pull` into the AxVisor CLI path.
- Updates the QEMU AArch64 quick-start image bundle paths.

Recommendation:

- Treat this as infrastructure polish. It is helpful, but it is not necessary
  for the contest demo evidence or the dual-guest reproduction script.

Observed validation:

```text
CARGO_BUILD_JOBS=1 cargo test -p axbuild image::tests::parses_pull_by_arch --lib
CARGO_BUILD_JOBS=1 cargo test -p axbuild image::storage::tests::pull_rootfs_image_returns_extracted_rootfs_file --lib
```

Both targeted axbuild tests passed (`1/1` each).

## Current Worktree Revalidation

The current uncommitted core worktree was rechecked on 2026-07-28 after the
contest-material package reached the 38-file boundary.

```text
cargo fmt --check -p arm_vcpu -p arm_vgic -p axvmconfig -p axvm -p axbuild: PASS
CARGO_BUILD_JOBS=1 cargo test -p axvmconfig -p axvm -p arm_vgic --lib: PASS
  arm_vgic: 5/5
  axvm: 110/110
  axvmconfig: 18/18
CARGO_BUILD_JOBS=1 cargo test -p arm_vcpu --lib: PASS
  arm_vcpu: 0/0 host-side tests
CARGO_BUILD_JOBS=1 cargo test -p axbuild image::tests::parses_pull_by_arch --lib: PASS
  axbuild targeted test: 1/1
CARGO_BUILD_JOBS=1 cargo test -p axbuild image::storage::tests::pull_rootfs_image_returns_extracted_rootfs_file --lib: PASS
  axbuild targeted test: 1/1
cargo fmt --check -p somehal: PASS
CARGO_BUILD_JOBS=1 cargo check -p somehal --features hv --target aarch64-unknown-none-softfloat: PASS
```

Warnings observed during these gates were limited to existing `dead_code`
warnings in test/build configurations. They did not fail the gates. The current
branch still has `0` staged paths; these checks do not authorize mixing core
changes into the first-stage contest-material commit.

## Commit Boundary Reminder

Do not use `git add .` in the current repository state. There are generated
images, temporary experiment helpers, and unreviewed core changes in the work
tree. Use explicit path lists for each commit group.
