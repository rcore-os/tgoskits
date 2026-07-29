# PR Boundary Notes

This note records the current redcola submission boundary for the Quancheng Lab
2026 AxVisor contest work. It is meant to keep the deliverable PR reviewable and
to avoid mixing verified contest artifacts with temporary experiment files.

## Current Safe Boundary

The safe first-stage boundary is:

```text
os/axvisor/contest/quancheng2026/
```

This directory contains the contest-facing source, scripts, RTOS patch, protocol
document, reproduction notes, demo script, status report, formal realtime
comparison, core patch review notes, and compact stability summaries.
It should be reviewable independently from the AxVisor core changes.

Current validation for this boundary:

- Python syntax: `python3 -m py_compile scripts/*.py linux/*.py` passed.
- Shell syntax: `bash -n scripts/*.sh linux/*.sh` passed.
- Generated-artifact scan found no `.img`, `.qcow2`, `.iso`, `.elf`, `.o`,
  `.bin`, `.log`, `.tar.gz`, `__pycache__`, or `.pyc` files.
- Template-regression scan found no stale PowerShell template fragments.
- Stale 3-run follow-up scan found no "still need to add 3 rounds" wording.
- `git add --dry-run -- os/axvisor/contest/quancheng2026` would add `38`
  files, all under this contest directory; the staged diff is empty before the
  real add.
- The 2-worker 3-run stability evidence is recorded under
  `results/stability/2026-07-27-stress2-3x/`.

If making a first-stage commit, use an explicit path:

```bash
git add -- os/axvisor/contest/quancheng2026
```

Do not use `git add .` for this repository state.

## Contest Artifacts In Scope

The following groups are suitable for the first-stage contest-material commit:

- `README.md`: current evidence index and directory layout.
- `docs/protocol.md`: QCZ1 reliable UDP/IP application protocol.
- `docs/reproduce.md`: native Zephyr and AxVisor dual-guest reproduction notes.
- `docs/demo-video-script.md`: 5-minute final demo recording script with
  narration, commands, and required PASS markers.
- `docs/e1000_axvisor.md`: AxVisor-hosted Zephyr e1000 validation notes.
- `docs/realtime-evaluation.md`: Task-One realtime comparison, measurement
  scope, platform-difference notes, and reviewer-facing conclusions.
- `docs/core-patch-review.md`: recommended split for the current AxVisor core
  patch candidates and their validation/risk notes.
- `docs/pr-boundary.md`: this PR boundary note.
- `linux/`: Linux guest probes, QCZ1 client, AI control demo, periodic latency
  probe, and guest init script.
- `rtos/`: Zephyr IPv4-only config, protocol patch, and full UDP server snapshot.
- `scripts/`: native and AxVisor reproduction scripts, protocol probes, and
  analyzers.
- `results/CURRENT_STATUS_2026-07-26.md`: consolidated status and evidence map.
- `results/realtime-comparison.csv`: compact 0/1/2/4-worker and 3-run realtime
  comparison table.
- `results/stability/2026-07-27-stress2-3x/`: compact 3-run stability summary.

These files directly support contest Task 1 realtime validation, Task 2
Linux/RTOS IP communication, and Task 3 AI-to-RTOS closed-loop control.

## Core AxVisor Patch Candidates

The repository also has useful but not-yet-separated AxVisor core changes. These
should be reviewed as a second-stage patch, not bundled blindly with the contest
materials.

### Explicit Passthrough IRQ Configuration

Candidate files:

```text
os/axvisor/src/config.rs
virtualization/axvm/src/config.rs
virtualization/axvmconfig/src/lib.rs
virtualization/axvmconfig/src/templates.rs
virtualization/axvmconfig/src/test.rs
```

Purpose:

- Adds `passthrough_irqs` to VM device configuration.
- Deduplicates explicit IRQ IDs when constructing `AxVMConfig`.
- Copies the explicit IRQ list from AxVisor crate config into AxVM runtime
  config.
- Adds unit coverage for config parse/default/conversion behavior.

Observed validation:

- `cargo test -p axvmconfig -p axvm -p arm_vgic --lib` passed, including
  `axvmconfig 18/18` and `axvm 110/110`.

### Physical Timer / vTimer Support

Candidate files:

```text
virtualization/arm_vgic/src/vtimer/cntp_cval_el0.rs
virtualization/arm_vgic/src/vtimer/cntp_timer.rs
virtualization/arm_vgic/src/vtimer/cntp_ctl_el0.rs
virtualization/arm_vgic/src/vtimer/cntp_tval_el0.rs
virtualization/arm_vgic/src/vtimer/mod.rs
virtualization/axdevice/src/adapter.rs
```

Purpose:

- Adds guest-visible `CNTP_CVAL_EL0`.
- Makes `CNTP_CTL_EL0`, `CNTP_TVAL_EL0`, and `CNTP_CVAL_EL0` share one timer
  state.
- Rearms a host timer and injects virtual physical timer PPI 30 when the guest
  physical timer expires.
- Keeps unit tests for shared timer state and tick-to-nanosecond conversion.

Observed validation:

- `cargo test -p axvmconfig -p axvm -p arm_vgic --lib` passed, including
  `arm_vgic 5/5`.

Review note:

- Host-side timer helpers are only used on `target_arch = "aarch64"`, so
  host-architecture unit tests may still report dead-code warnings. These are
  warnings, not test failures.

### GIC / Interrupt Path Diagnostics

Candidate files:

```text
platforms/somehal/src/arch/aarch64/gic/v3.rs
virtualization/arm_vcpu/src/context_frame.rs
virtualization/arm_vcpu/src/exception.rs
virtualization/arm_vcpu/src/vcpu.rs
virtualization/arm_vgic/src/v3/vgicd.rs
```

Purpose:

- Changes GICv3 CPU interface EOI mode under `hv`.
- Adds bounded guest register diagnostic snapshots.
- Adds bounded synchronous-exception, IRQ, and vGICD access traces.

Review note:

- These changes were valuable for isolating the Zephyr e1000 and Linux/RTOS
  dual-guest interrupt path.
- The high-frequency synchronous-exception, EL2, and vGICD diagnostics have
  been reduced from `info` to bounded `debug` logging.
- Observed validation after the logging cleanup: `cargo fmt --check -p
  arm_vcpu -p arm_vgic -p axvmconfig -p axvm -p axbuild` passed,
  `cargo test -p arm_vgic --lib` passed (`5/5`), and
  `cargo test -p arm_vcpu --lib` passed.

### Axbuild Image Helper / Quick Start

Candidate files:

```text
scripts/axbuild/src/axvisor/mod.rs
os/axvisor/scripts/quick-start.sh
```

Purpose:

- Wires the `cargo axvisor image pull` subcommand into the AxVisor CLI path.
- Updates QEMU AArch64 quick-start image paths for the current image bundle.

Observed validation:

- `CARGO_BUILD_JOBS=1 cargo test -p axbuild image::tests::parses_pull_by_arch --lib` passed.
- `CARGO_BUILD_JOBS=1 cargo test -p axbuild image::storage::tests::pull_rootfs_image_returns_extracted_rootfs_file --lib` passed.

Review note:

- This is useful infrastructure, but it is not essential to the contest demo.
  It can be split into a separate commit if the main PR needs to stay focused.

## Out Of Scope For First-Stage Commit

Do not stage these paths without a separate review:

```text
os/axvisor/images/
scripts/build_zephyr_echo_server_e1000_fixed_0x80000000.sh
scripts/build_zephyr_echo_server_virtio_net_bus23_fixed_0x90000000.sh
scripts/prepare_linux_dual_guest_udp_rootfs.sh
scripts/qc_dual_guest_udp_echo_probe.c
scripts/qc_dual_guest_udp_init.sh
scripts/run_axvisor_linux_zephyr_dual_guest_udp_echo.sh
scripts/run_axvisor_zephyr_e1000_fixed_identity_diag.sh
scripts/run_axvisor_zephyr_virtio_net_usernet_cpu0_diag.sh
scripts/run_axvisor_zephyr_virtio_net_usernet_probe.sh
scripts/run_zephyr_echo_server_e1000_arp_ab.sh
```

Reason:

- These are experiment helpers, generated image metadata, or earlier diagnostic
  scripts. Their useful logic has been condensed into
  `os/axvisor/contest/quancheng2026/` where possible.

## Recommended Commit Sequence

1. Contest material commit:

```bash
git add -- os/axvisor/contest/quancheng2026
git diff --cached --stat
git diff --cached --name-status
```

2. Core VM config and vTimer commit, after review:

```bash
git add -- \
  os/axvisor/src/config.rs \
  virtualization/axvm/src/config.rs \
  virtualization/axvmconfig/src/lib.rs \
  virtualization/axvmconfig/src/templates.rs \
  virtualization/axvmconfig/src/test.rs \
  virtualization/arm_vgic/src/vtimer/cntp_cval_el0.rs \
  virtualization/arm_vgic/src/vtimer/cntp_timer.rs \
  virtualization/arm_vgic/src/vtimer/cntp_ctl_el0.rs \
  virtualization/arm_vgic/src/vtimer/cntp_tval_el0.rs \
  virtualization/arm_vgic/src/vtimer/mod.rs \
  virtualization/axdevice/src/adapter.rs
```

3. GIC/vCPU diagnostics commit only if the bounded debug diagnostics are still
   needed for reviewer evidence or can be framed as a controlled debug feature.

4. Axbuild/quick-start helper commit only if image-management changes are part
   of the intended submission story.

Before each commit, rerun the boundary checks and inspect the staged diff.
