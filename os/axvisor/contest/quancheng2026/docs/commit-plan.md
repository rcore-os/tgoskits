# Commit Plan

This note records the current first-stage commit preflight for the redcola
Quancheng Lab 2026 AxVisor contest artifacts.

## First-Stage Commit Scope

The first-stage commit should contain only:

```text
os/axvisor/contest/quancheng2026/
```

Dry-run command:

```bash
git add --dry-run -- os/axvisor/contest/quancheng2026
```

Expected dry-run result after adding this note:

- `38` files would be added.
- All files are under `os/axvisor/contest/quancheng2026/`.
- No AxVisor core files, image files, temporary scripts, or generated logs would
  be staged by this command.

## Files To Add

```text
os/axvisor/contest/quancheng2026/README.md
os/axvisor/contest/quancheng2026/docs/ai-control-evaluation.md
os/axvisor/contest/quancheng2026/docs/core-patch-review.md
os/axvisor/contest/quancheng2026/docs/design.md
os/axvisor/contest/quancheng2026/docs/demo-video-script.md
os/axvisor/contest/quancheng2026/docs/e1000_axvisor.md
os/axvisor/contest/quancheng2026/docs/final-submission-checklist.md
os/axvisor/contest/quancheng2026/docs/commit-plan.md
os/axvisor/contest/quancheng2026/docs/network-topology.md
os/axvisor/contest/quancheng2026/docs/pr-boundary.md
os/axvisor/contest/quancheng2026/docs/pr-description.md
os/axvisor/contest/quancheng2026/docs/protocol.md
os/axvisor/contest/quancheng2026/docs/realtime-evaluation.md
os/axvisor/contest/quancheng2026/docs/reproduce.md
os/axvisor/contest/quancheng2026/docs/scorecard-traceability.md
os/axvisor/contest/quancheng2026/docs/test-report.md
os/axvisor/contest/quancheng2026/linux/qc_ai_control_demo.py
os/axvisor/contest/quancheng2026/linux/qc_dual_guest_qcz1_ai_init.sh
os/axvisor/contest/quancheng2026/linux/qc_dual_guest_udp_echo_probe.c
os/axvisor/contest/quancheng2026/linux/qc_periodic_latency_probe.c
os/axvisor/contest/quancheng2026/linux/qc_qcz1_guest_demo.c
os/axvisor/contest/quancheng2026/linux/qc_reliable_udp_client.py
os/axvisor/contest/quancheng2026/results/CURRENT_STATUS_2026-07-26.md
os/axvisor/contest/quancheng2026/results/realtime-comparison.csv
os/axvisor/contest/quancheng2026/results/stability/2026-07-27-stress2-3x/stability-summary.csv
os/axvisor/contest/quancheng2026/results/stability/2026-07-27-stress2-3x/stability-summary.md
os/axvisor/contest/quancheng2026/rtos/zephyr_ipv4only_udp_mgmt12288.conf
os/axvisor/contest/quancheng2026/rtos/zephyr_udp_qc_protocol.patch
os/axvisor/contest/quancheng2026/rtos/zephyr_udp_qc_protocol_udp.c
os/axvisor/contest/quancheng2026/scripts/analyze_dual_guest_realtime.py
os/axvisor/contest/quancheng2026/scripts/analyze_zephyr_latency_measure.py
os/axvisor/contest/quancheng2026/scripts/qc_ai_control_combined_probe.py
os/axvisor/contest/quancheng2026/scripts/qc_reliable_udp_combined_probe.py
os/axvisor/contest/quancheng2026/scripts/qc_udp_echo_probe.py
os/axvisor/contest/quancheng2026/scripts/run_axvisor_dual_guest_qcz1_ai.sh
os/axvisor/contest/quancheng2026/scripts/run_native_zephyr_latency_baseline.sh
os/axvisor/contest/quancheng2026/scripts/run_native_zephyr_mgmt_stack_2048_nogdb_validation.sh
os/axvisor/contest/quancheng2026/scripts/run_native_zephyr_serial_validation_campaign.sh
```

## Files To Keep Out

Modified core files that should stay unstaged for the first-stage commit:

```text
os/axvisor/scripts/quick-start.sh
os/axvisor/src/config.rs
platforms/somehal/src/arch/aarch64/gic/v3.rs
scripts/axbuild/src/axvisor/mod.rs
virtualization/arm_vcpu/src/context_frame.rs
virtualization/arm_vcpu/src/exception.rs
virtualization/arm_vcpu/src/vcpu.rs
virtualization/arm_vgic/src/v3/vgicd.rs
virtualization/arm_vgic/src/vtimer/cntp_ctl_el0.rs
virtualization/arm_vgic/src/vtimer/cntp_tval_el0.rs
virtualization/arm_vgic/src/vtimer/mod.rs
virtualization/axdevice/src/adapter.rs
virtualization/axvm/src/config.rs
virtualization/axvmconfig/src/lib.rs
virtualization/axvmconfig/src/templates.rs
virtualization/axvmconfig/src/test.rs
```

Non-contest untracked files that should stay unstaged for the first-stage
commit:

```text
os/axvisor/images/qemu-aarch64/zephyr-virtio-net-bus23/entry-point.txt
os/axvisor/images/qemu-aarch64/zephyr-virtio-net-bus23/zephyr.config
os/axvisor/images/qemu-aarch64/zephyr-virtio-net-bus23/zephyr.dts
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
virtualization/arm_vgic/src/vtimer/cntp_cval_el0.rs
virtualization/arm_vgic/src/vtimer/cntp_timer.rs
```

The two new vTimer source files are useful core-patch candidates, but they
belong with the second-stage core patch rather than this contest-material
commit.

## Pre-Commit Checks

Run from:

```bash
REPO=/path/to/tgoskits
cd "${REPO}/os/axvisor/contest/quancheng2026"
```

Checks:

```bash
find . -type d -name __pycache__ -prune -exec rm -rf {} +
cache_dir=/tmp/qc_pycompile_cache_$$
PYTHONPYCACHEPREFIX=$cache_dir python3 -m py_compile scripts/*.py linux/*.py
rm -rf $cache_dir
python3 scripts/qc_qcz1_guest_status_negative_selftest.py
bash -n scripts/*.sh linux/*.sh
find . \( -name '*.img' -o -name '*.qcow2' -o -name '*.iso' -o -name '*.elf' -o -name '*.o' -o -name '*.bin' -o -name '__pycache__' -o -name '*.pyc' -o -name '*.log' -o -name '*.tar.gz' \) -print | sort
grep -R -E '[$][(]@|@[{]' README.md docs results scripts linux || true
python3 - <<'PY'
from pathlib import Path
import re

pattern = re.compile(
    "\u4e0b\u4e00\u6b65\u8865\\s*3\\s*\u8f6e|"
    "\u540e\u7eed.*\u8865\\s*3\\s*\u8f6e"
)
for root in [Path("README.md"), Path("docs"), Path("results")]:
    paths = [root] if root.is_file() else sorted(p for p in root.rglob("*") if p.is_file())
    for path in paths:
        for line_no, line in enumerate(path.read_text(encoding="utf-8", errors="replace").splitlines(), 1):
            if pattern.search(line):
                print(f"{path}:{line_no}:{line}")
PY
```

Expected result:

- Python and shell checks return `0`.
- Artifact scan prints no files after `__pycache__` cleanup.
- Template and stale-3-run scans print no matches.

## Stage And Inspect

Only after the checks pass:

```bash
REPO=/path/to/tgoskits
cd "${REPO}"
git add -- os/axvisor/contest/quancheng2026
git diff --cached --stat
git diff --cached --name-status
```

Expected staged paths:

- Every staged path starts with `os/axvisor/contest/quancheng2026/`.
- Staged count is `38` files unless this document or another contest document is
  updated again.

If any staged path is outside the contest directory, stop and unstage it before
committing.

## Suggested Commit Message

```text
contest: add quancheng2026 AxVisor validation artifacts

Add redcola contest materials for the Quancheng Lab 2026 AxVisor task:

- Zephyr IPv4/e1000 RTOS baseline and protocol patch
- design and test-report entry documents for contest submission review
- Linux/RTOS reliable UDP QCZ1 protocol probes
- explicit dual-guest network topology and access-boundary notes
- AI-to-RTOS control closed-loop demo
- AI/manual fixed-gain comparison notes
- dual-guest AxVisor reproduction and analysis scripts
- formal realtime comparison, reliability, and 3-run stability evidence summaries
- PR boundary, scorecard, core patch review, demo script, and reproduction notes
- final submission checklist and PR description draft

Validation:
- PYTHONPYCACHEPREFIX=/tmp/qc_pycompile_cache python3 -m py_compile scripts/*.py linux/*.py
- python3 scripts/qc_qcz1_guest_status_negative_selftest.py
- bash -n scripts/*.sh linux/*.sh
- cargo fmt --check -p arm_vcpu -p arm_vgic -p axvmconfig -p axvm -p axbuild
- cargo test -p axvmconfig -p axvm -p arm_vgic --lib
- cargo test -p arm_vcpu --lib
- CARGO_BUILD_JOBS=1 cargo test -p axbuild image::tests::parses_pull_by_arch --lib
- CARGO_BUILD_JOBS=1 cargo test -p axbuild image::storage::tests::pull_rootfs_image_returns_extracted_rootfs_file --lib
```

## Suggested PR Summary

```text
This PR adds the redcola Quancheng Lab 2026 AxVisor contest artifact directory.
It collects the Linux/RTOS reliable UDP protocol implementation, Zephyr RTOS
patch/config, AI control demo, dual-guest reproduction script, realtime and
communication analyzers, and compact validation summaries.

The contest artifacts are intentionally isolated under
os/axvisor/contest/quancheng2026/ so they can be reviewed separately from the
AxVisor core scheduler, interrupt, timer, and vCPU experiments.
```
