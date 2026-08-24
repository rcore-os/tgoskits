# Final evidence manifest (2026-08-21)

This is the human-readable index for the current delivery branch. Hashes are
SHA-256 of the tracked files in the delivery branch at the time of this
manifest refresh and are included to prevent an old run being silently
substituted for the current evidence.

## Canonical design and score documents

| Artifact | SHA-256 |
|---|---|
| `results/task1/README.md` | `84e13805af4df1ca14f4cafd03b1b2dc61411099b7bc590de20ea7d5f4962a24` |
| `results/task1/two-gap-closure-20260820.md` | `51f2ee2f057ec5f2708dfe2e6c2528e049373479646052c26b919e32011a0440` |
| `results/task1/irq-tail-preemption-design.md` | `cac0440a2a4d25c3907a6063991fb4fdc82929b4f2db55ba5f45714322c02195` |
| `results/final-execution-todo-20260821.md` | `456ddb774d074aff544cbb3dc5934c9e5a945ad5bfdd0295dbf7f3d08ea9c577` |
| `results/final-submission-scorecard-20260821.md` | `839ad11a00f5e94dff8ab0357b58b5b94875c0ab61983fc80d43617b15b405ff` |
| `results/task2-final-run-20260821.md` | `2d8046ab9456c3b21cb2b1c288fe94415b04b87b08829ee10c3f7336e7f96a5a` |
| `book/design/task2-dual-guest-network-final.md` | `4cd228e46fc1aeb3e8e3cd98b0429cc2ab95e33318c886e8ae71d97c34d489fe` |
| `book/design/task3-ai-design.md` | `d8ca620893f9066c5a9b9ecce6ee0620916d014ce52e80fcc332c2c5eb2dafc2` |
| `results/task3/README.md` | `931a6a12f6ea7758241f4c7614d0367de77720c925e186eec27ed252c4493767` |
| `results/bonus-path-audit-20260821.md` | `8cae078fc190e4f45aea1ae566b40bd6a4201d99d25f03a8b1dbc495b4f197db` |
| `results/demo-runbook-20260821.md` | `39fc866b66923d50ba117b7077987ab517694a454584da0b03cdde40135386b2` |

## Current HEAD Task3 evidence

| Run | Evidence directory | Required verifier |
|---|---|---|
| Normal YOLO | `results/task3/switch/current-head-yolo-capture/` | `verify_pcap.py --require-task2` |
| Normal YOLO (final-head replay v2) | `results/task3/switch/final-head-yolo-replay-v2/` | `verify_pcap.py --require-task2` |
| YOLO blackout/recovery | `results/task3/switch/fault-current-head-yolo-fault-validated/` | marker order + `verify_pcap.py --require-task2` |
| YOLO blackout/recovery (final-head v2) | `results/task3/switch/fault-final-head-yolo-blackout-v2/` | marker order + `verify_pcap.py --require-task2` |
| YOLO ACK drop | `results/task3/fault-current-head-yolo-ack-drop-v2/` | `verify_fault_pcap.py` |
| YOLO out-of-order | `results/task3/fault-current-head-yolo-injection-out-of-order/` | `verify_protocol_injection.py --mode out-of-order` |
| YOLO invalid parameter | `results/task3/fault-current-head-yolo-injection-invalid-parameter-v2/` | `verify_protocol_injection.py --mode invalid-parameter` |
| YOLO out-of-order (final HEAD replay) | `results/task3/fault-current-head-yolo-injection-out-of-order-v2/` | `verify_protocol_injection.py --mode out-of-order` |

All seven directories contain the run/build or guest/proxy logs, pcaps where
applicable, and a manifest with input/output hashes. The two protocol-injection
runs were executed on the QEMU wire and passed their dedicated verifier.
The ACK-drop capture intentionally does not pass the normal equal-ledger
verifier because it contains the one expected missing ACK; it passes
`verify_fault_pcap.py`, which checks the exact one-frame delta plus retransmit and
duplicate markers.

The final-head replay v2 normal run used the slot-0 Zephyr image with SHA-256
`49ca61bc847835e61a03f94fb619fca01b49f02157d347b0fc0a9806f7fcb433`; its
manifest records 320 frames per side and 315 verified T2N1 frames. The final-head
blackout v2 run used the same Zephyr image and records the ordered
blackout/Safe/recovery markers, 45 s resumed operation, and 727 frames per side.
The normal replay was run before evidence-only commit `78c74716b`; the blackout
replay was run with the model-selecting fault-runner code at `479627c46`. Those
later commits only archive evidence or update documentation; they do not change
the Guest protocol, model contract, hypervisor, or image-building behavior
exercised by the runs.

Historical parent captures retained for comparison:
Most runtime captures record `git_head=5bb5c7957` in their own manifests. The
`out-of-order-v2` capture was replayed at final HEAD `92a97d12f`. Other commits
after that point only archived existing captures or changed verifier/docs; they
did not change the Guest protocol, model, hypervisor, or image-building code.
Strict proof against a future rebased/integrated PR head still requires one fresh
full QEMU run after that integration; this manifest does not silently upgrade
the parent-commit captures into such proof.

## Reproduction gates

```bash
cargo test -p task2-net-protocol
python3 -m unittest discover -s scripts/test/net-dual-guest -p 'test_*.py'
bash -n scripts/task3/run-task3-fault.sh
bash -n scripts/task3/run-task3-switch-fault.sh
python3 -m py_compile scripts/test/net-dual-guest/verify_protocol_injection.py
git diff --check -- book/design results scripts/task3 scripts/test/net-dual-guest
```

The full dual-Guest QEMU runs are intentionally explicit rather than silently
treated as a host-only unit test. QEMU SIL measurements are not physical-board
WCET claims, and `embedded:fixture-replay` is not an ONNX-runtime benchmark.

## Delivery limitations

- StarryOS/STERRORS and a second RTOS/board remain separately audited in
  `results/bonus-path-audit-20260821.md`; no bonus is claimed without a matching
  observable protocol/control loop.
- `origin/dev` has conflicts with this long-lived evidence branch. A read-only
  `git merge-tree --write-tree HEAD origin/dev` audit reports conflicts in the
  realtime test helpers and AxVM AArch64/FDT/vCPU files; integration must be
  performed in a review branch, not by silently rewriting this evidence branch.
