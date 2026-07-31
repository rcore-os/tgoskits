# Scorecard Traceability Matrix

This document maps the Quancheng Lab 2026 AxVisor contest requirements to the
current redcola evidence set. It is written as a reviewer-facing checklist so
the final PR, weekly report and demo can point to one compact source of truth.

## Executive Status

| Area | Status | Primary evidence |
| --- | --- | --- |
| Task One realtime | PASS | `docs/realtime-evaluation.md`, `results/realtime-comparison.csv`, native Zephyr latency baseline and 0/1/2/4-worker dual-guest runs |
| Task Two IP communication | PASS | `docs/protocol.md`, `docs/network-topology.md`, Linux client scripts, Zephyr e1000 RTOS path, tcpdump counters |
| Task Three AI closed loop | PASS | `docs/ai-control-evaluation.md`, `linux/qc_ai_control_demo.py`, integrated QCZ1 AI run summaries |
| Reproducibility | PASS | `docs/reproduce.md`, downloadable-artifact integrated runner, analyzer scripts, SHA256 evidence records |
| Submission boundary | READY | `docs/commit-plan.md`, `docs/pr-boundary.md`, contest dry-run path check |

## Task One: Realtime Modification and Validation

| Requirement | Current evidence | Reviewer note |
| --- | --- | --- |
| Improve AxVisor realtime-related paths | Core patch candidates split into VM config + vTimer, GIC EOI mode, bounded diagnostics and axbuild helper. | `docs/core-patch-review.md` explains patch order and risk. First-stage commit keeps contest material separate from core code. |
| Boot a multi-vCPU Linux guest | Integrated run reports Linux guest `2` vCPUs online. | `docs/design.md` records the guest placement and the Linux guest role. |
| Describe vCPU/physical CPU binding, memory, devices, interrupts and boot args | Guest and topology docs describe Linux `2` vCPU setup, the per-run isolated TAP/bridge network, virtio-net, Zephyr e1000, interrupt route and `noirqdebug` run. | See `docs/design.md`, `docs/network-topology.md` and `docs/realtime-evaluation.md`. |
| Measure periodic jitter, scheduling latency, interrupt responsiveness, max latency and stability | Linux 1 ms periodic probe, RTOS 1 ms periodic probe, 0/1/2/4-worker stress runs, and 2-worker 3-run stability campaign are recorded. | Main comparison table is `results/realtime-comparison.csv`; stability summary is `results/stability/2026-07-27-stress2-3x/stability-summary.md`. |
| Compare against native RTOS baseline | Native Zephyr latency baseline passed with 47 metrics, including context switch `2400 ns` and max primitive latency `46703 ns`. | Platform differences and measurement limits are documented in `docs/realtime-evaluation.md`. |
| Provide reproducible branch, images, configs, commands and scripts | Reproduction commands and evidence paths are listed in `docs/reproduce.md`. | Large images and raw evidence archives stay outside the source commit and are referenced by path/SHA256. |

## Task Two: Linux/RTOS IP Communication

| Requirement | Current evidence | Reviewer note |
| --- | --- | --- |
| Use IP protocol stack as the main data channel | The main channel is IPv4/UDP over a per-run isolated TAP/bridge network, with Linux virtio-net and Zephyr e1000. | `docs/network-topology.md` states that shared memory, HyperCall, MMIO and vsock are not the primary data path. |
| Provide bidirectional Linux/RTOS application protocol | QCZ1 supports command, state reply and error/fault style response frames. | Full frame format is in `docs/protocol.md`. |
| Include version, type, payload length, sequence/timestamp and checksum/error field | QCZ1 frame includes magic/version/type/flags/header length/payload length/sequence/timestamp/status/checksum. | This directly addresses the protocol-field checklist. |
| Reliability for UDP | Linux client supports ACK handling, timeout, retry, duplicate ACK accounting and response validation. | Integrated runs report `10/10 PASS`, duplicate ACK count and retransmit count. |
| Document topology, MAC/IP, route, port and access boundary | Linux `192.0.2.10`, Zephyr `192.0.2.20`, UDP `4242`, bridge/TAP layout and no-NAT boundary are documented. | `docs/network-topology.md` is the reviewer entry point. |
| Report success rate, errors, timeouts, recovery, latency and throughput/counters | Plain UDP `20/20`, QCZ1 `10/10`, tcpdump `0` kernel drops and RTT summaries are recorded across clean/stress/stability runs. | Main summaries are in `README.md`, `docs/test-report.md` and `results/CURRENT_STATUS_2026-07-26.md`. |

## Task Three: AI Model and Control Linkage

| Requirement | Current evidence | Reviewer note |
| --- | --- | --- |
| Deploy neural-network inference in Linux guest | Linux-side AI control demo uses a deterministic small MLP implemented in the static guest demo/client path. | The model is intentionally lightweight and reproducible for the contest demo. |
| Send model output to RTOS over Task Two protocol | AI output `ai_score_milli` is carried in QCZ1 AI/control payloads over IPv4/UDP. | See `docs/ai-control-evaluation.md` and `docs/protocol.md`. |
| RTOS adjusts observable control parameter or strategy | RTOS applies `output_milli = setpoint_milli * ai_score_milli / 1000` and reports state back. | Current observable output is logs/state response; it can be extended to LED/PWM on hardware. |
| Demonstrate closed loop | Integrated run includes AI input, inference, QCZ1 network transfer, RTOS control output and state reply. | The downloadable-artifact runner requires `QC_AI_CONTROL_RESULT=PASS`. |
| Measure end-to-end latency | Clean run reports AI end-to-end mean `2.186 ms`, max `3.389 ms`; 4-worker overcommit run reports mean `8.140 ms`, max `39.642 ms`. | Measurement method and precision notes are in `docs/ai-control-evaluation.md`. |
| Compare against fixed manual baseline with at least two metrics | Manual fixed-gain baseline uses `manual_score_milli = 800`; comparison uses latency and control error/output tracking. | Representative values are in `docs/ai-control-evaluation.md`. |

## Submission Materials

| Required material | Current location | Status |
| --- | --- | --- |
| Design document | `docs/design.md` plus linked protocol/topology/realtime docs | READY |
| Test document | `docs/test-report.md` plus `results/realtime-comparison.csv` and stability summary | READY |
| Source code | Artifact PR `#1703`; core vTimer/GIC/IRQ support in upstream via PR `#1770`; StarryOS bonus clean branch prepared separately | SUBMITTED / READY |
| Reproduction instructions | `docs/reproduce.md` | READY |
| Demo video | `docs/demo-video-script.md` | SCRIPT READY; recording still needs user-side capture |
| PR form | `docs/commit-plan.md`, `docs/pr-boundary.md`, PR `#1703`, merged core PR `#1770` | SUBMITTED |

## Award-Oriented Positioning

| Scoring item | Evidence angle |
| --- | --- |
| Technical innovation 30% | Mixed Linux/RTOS AxVisor deployment, Zephyr e1000 guest networking, reliable QCZ1 protocol, AI control loop and separated core patch candidates. |
| Completeness 30% | Covers realtime, IP communication, AI linkage, reproducibility, static checks, evidence SHA256 and demo script. |
| Landing feasibility 25% | Uses a downloadable-artifact QEMU reproduction path, explicit topology, deterministic model and clear first-stage/core-patch separation. |
| Team capability 15% | Provides runnable code, measured results under stress, stability evidence, risk notes and PR-ready staging discipline. |

## Remaining Actions Before Final Submission

1. Wait for review/CI refresh on artifact PR `#1703` after the latest runtime-contract fix.
2. Open the StarryOS bonus PR from `contest/starry-redcola-ai-bonus-clean-20260731` as the separate StarryOS add-on review unit.
3. Record the 5-minute demo video using `docs/demo-video-script.md`.
4. Package the final platform submission with PR links, design/test/reproduce docs, evidence SHA256 values and the demo video.
