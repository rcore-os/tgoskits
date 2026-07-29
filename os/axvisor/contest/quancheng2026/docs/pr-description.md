# PR Description Draft

## Summary

This PR adds the redcola Quancheng Lab 2026 AxVisor contest artifact directory
under `os/axvisor/contest/quancheng2026/`.

The directory contains a reviewer-ready mixed-criticality demo package:

- Zephyr RTOS IPv4/e1000 networking configuration and protocol patch.
- Linux/RTOS QCZ1 reliable UDP protocol clients, probes and analyzers.
- Linux-side lightweight neural-network control demo and RTOS control response.
- AxVisor dual-guest reproduction script for Linux plus Zephyr RTOS.
- Native Zephyr realtime baseline, AxVisor dual-guest realtime summaries and
  stress/stability evidence tables.
- Design, test, topology, protocol, AI evaluation, reproducibility, demo,
  scorecard and core-patch review documents.

The contest material is intentionally isolated from the AxVisor core changes so
reviewers can inspect the evidence and scripts first. Core VM config, vTimer,
GIC EOI-mode and diagnostic changes are documented separately in
`docs/core-patch-review.md`.

## Contest Task Mapping

Task One, realtime:

- Native Zephyr latency baseline passed with 47 metrics.
- AxVisor dual-guest runs collect Linux and RTOS 1 ms periodic probes.
- 0/1/2/4-worker stress results and a 2-worker 3-run stability summary are
  included.

Task Two, inter-guest communication:

- The main data path is IPv4/UDP over a per-run isolated TAP/bridge network.
- Linux guest uses `192.0.2.10`; Zephyr RTOS guest uses `192.0.2.20`.
- QCZ1 provides versioned, checksummed, sequenced request/response frames over
  UDP with timeout/retry handling.

Task Three, AI control loop:

- Linux-side demo runs a deterministic small neural network.
- The model output is sent as `ai_score_milli` over QCZ1.
- RTOS computes and reports `output_milli` from the received AI score.
- Integrated runs record AI end-to-end latency and compare with a fixed manual
  baseline.

## Validation

Contest artifact checks:

```text
python3 -m py_compile scripts/*.py linux/*.py
bash -n scripts/*.sh linux/*.sh
artifact scan for images/logs/pyc/tarballs: 0
git diff --check
git add --dry-run -- os/axvisor/contest/quancheng2026
```

Core worktree revalidation recorded in `docs/core-patch-review.md`:

```text
cargo fmt --check -p arm_vcpu -p arm_vgic -p axvmconfig -p axvm -p axbuild
CARGO_BUILD_JOBS=1 cargo test -p axvmconfig -p axvm -p arm_vgic --lib
CARGO_BUILD_JOBS=1 cargo test -p arm_vcpu --lib
CARGO_BUILD_JOBS=1 cargo test -p axbuild image::tests::parses_pull_by_arch --lib
CARGO_BUILD_JOBS=1 cargo test -p axbuild image::storage::tests::pull_rootfs_image_returns_extracted_rootfs_file --lib
cargo fmt --check -p somehal
CARGO_BUILD_JOBS=1 cargo check -p somehal --features hv --target aarch64-unknown-none-softfloat
```

## Review Notes

- The first-stage commit should include only `os/axvisor/contest/quancheng2026/`.
- Generated images, raw logs and large evidence archives are not committed.
- Raw evidence is referenced by directory path and SHA256 in the included docs.
- Core AxVisor changes should be reviewed as follow-up commits in the order
  documented in `docs/core-patch-review.md`.
