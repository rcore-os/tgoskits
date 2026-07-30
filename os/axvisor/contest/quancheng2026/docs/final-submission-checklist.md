# Final Submission Checklist

This checklist tracks the remaining steps from the current redcola contest
artifact state to the final Quancheng Lab submission.

## Current Ready State

| Item | Status | Evidence |
| --- | --- | --- |
| Zephyr e1000/IP baseline | READY | `docs/e1000_axvisor.md`, `rtos/zephyr_ipv4only_udp_mgmt12288.conf` |
| Linux/RTOS QCZ1 reliable UDP | READY | `docs/protocol.md`, `linux/qc_reliable_udp_client.py`, integrated run summaries |
| AI control closed loop | READY | `docs/ai-control-evaluation.md`, `linux/qc_ai_control_demo.py` |
| Realtime comparison | READY | `docs/realtime-evaluation.md`, `results/realtime-comparison.csv` |
| Stability summary | READY | `results/stability/2026-07-27-stress2-3x/stability-summary.md` |
| Reviewer scorecard | READY | `docs/scorecard-traceability.md` |
| Core patch review | READY | `docs/core-patch-review.md` |
| Demo video script | READY | `docs/demo-video-script.md` |
| PR description draft | READY | `docs/pr-description.md` |

## Current PR State

| Item | Status | Link or branch |
| --- | --- | --- |
| Contest artifact PR | SUBMITTED | `rcore-os/tgoskits#1703`, branch `contest/axvisor-2026` |
| Core vTimer PR | SUBMITTED | `rcore-os/tgoskits#1770`, head `40fa4b6e63258a5f452ad8cc74b61b0084c6d7c0` |
| StarryOS bonus branch | READY | `irinaparchina-art:contest/starry-redcola-ai-bonus-rebased-20260730`, head `db121505f0659d6370e75a72ce0faf56d2c61d2c` |

## Artifact PR Preflight

Run from the repository root:

```bash
REPO=/path/to/tgoskits
cd "${REPO}/os/axvisor/contest/quancheng2026"
find . -type d -name __pycache__ -prune -exec rm -rf {} +
cache_dir=/tmp/qc_pycompile_cache_$$
PYTHONPYCACHEPREFIX=$cache_dir python3 -m py_compile scripts/*.py linux/*.py
rm -rf $cache_dir
bash -n scripts/*.sh linux/*.sh
find . \( -name '*.img' -o -name '*.qcow2' -o -name '*.iso' -o -name '*.elf' -o -name '*.o' -o -name '*.bin' -o -name '__pycache__' -o -name '*.pyc' -o -name '*.log' -o -name '*.tar.gz' \) -print | sort
cd "${REPO}"
git diff --check upstream/dev...HEAD
git diff --name-only upstream/dev...HEAD | grep -v '^os/axvisor/contest/quancheng2026/' || true
```

Expected artifact PR result:

```text
tracked paths: 38
outside contest paths: 0
forbidden generated artifacts: 0
```

## StarryOS Bonus PR Candidate

The StarryOS bonus branch is intentionally separate from this artifact PR. It
adds only:

```text
apps/starry/qemu/redcola-ai-control/
```

It has a StarryOS AArch64 QEMU PASS log with a fixed-point MLP policy:

```text
EVDIR=/home/kali/qc-evidence/starry-qemu-redcola-ai-control-mlp-finalhead-20260731_033205
HEAD=db121505f0659d6370e75a72ce0faf56d2c61d2c
LOG_SHA256=56f604f15082ccd30e2bf49421b2ad64aa771e42b3ca90bcd70f4989b21bec2c
REDCOLA_STARRY_CONTROL_SUMMARY manual_abs_error=1013 ai_abs_error=0 max_ai_error=0 mean_infer_us=58
REDCOLA_STARRY_AI_CONTROL_PASS samples=8 manual_abs_error=1013 ai_abs_error=0 mean_infer_us=58
REDCOLA_STARRY_AI_DONE
RESULT=PASS
```

## Core Patch Tracking

The first core PR is `rcore-os/tgoskits#1770`. It covers AArch64 physical timer
virtualization and cross-CPU timer cancellation. Remaining optional core work,
if still useful after review, should stay in follow-up PRs rather than this
artifact PR.

The review reasoning, SHA256 values and validation commands are in
`docs/core-patch-review.md`.

## Demo Video

Record the final video with `docs/demo-video-script.md`.

Capture these markers:

```text
result=PASS
plain_udp=20/20
qcz1=10/10
ai_control=10/10
QC_RTOS_PERIODIC_RESULT=PASS
QC_DUAL_GUEST_LINUX_INIT=PASS
tcpdump kernel drops=0
```

## Final Platform Submission

Submit or link:

- PR branch and commit hash.
- `docs/design.md`.
- `docs/test-report.md`.
- `docs/reproduce.md`.
- `docs/scorecard-traceability.md`.
- Demo video.
- Latest source/documentation package SHA256.
