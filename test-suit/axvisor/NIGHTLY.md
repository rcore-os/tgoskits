# AxVisor Nightly

The `AxVisor Nightly` workflow runs the registered AxVisor CI checks daily at
19:20 UTC (03:20 Beijing time). It also supports `workflow_dispatch`: trigger it
from the GitHub Actions page ("AxVisor Nightly" → "Run workflow", choose `dev`)
or with `gh workflow run axvisor-nightly.yml --ref dev`. Both triggers test
`dev`; the planner resolves its commit once and every build and test checks out
that same SHA.

The workflow must be merged into the repository's default branch before the
scheduled run is available. Execution is limited to `rcore-os/tgoskits`, whose
self-hosted runners provide the required virtualization hosts and boards.

## Coverage

The source of truth is `.github/ci/checks/axvisor.toml`. Nightly runs every
enabled check in that catalog without changed-file filtering, including:

- AArch64 QEMU boot, timer stress, kernel tests, IVC, control plane and console.
- RISC-V QEMU boot and IPI cross-tests.
- LoongArch QEMU using the existing LVZ runner environment.
- Intel VMX and AMD SVM boot, ACPI and PCI tests.
- Registered board checks, including OrangePi Linux/Starry guests and the
  Zephyr-Starry IVC benchmark.

This is full coverage of the registered CI checks, not a claim that every
AxVisor feature or every test-suit directory has a corresponding nightly test.
Add new scenarios to the existing test-suit and register them in the catalog;
do not duplicate their shell commands in the nightly workflow.

## Default CI Versus Nightly

Default CI runs the functional checks. The GICv2/GICv3 timer stress and
OrangePi Zephyr-Starry IVC benchmark, single ArceOS guest performance and
Linux PCI network ping tests are separate checks marked
`nightly_only = true` in `axvisor.toml`. Nightly includes both functional and
nightly-only checks. Ordinary CI excludes nightly-only checks for PRs, pushes
and manual runs, even if the change precisely selects their test-suit files.
A PR changing only nightly test scenarios still receives static checks.
The default OrangePi Linux check explicitly selects only the `smoke` case.

To add another nightly-only scenario, register its own `[[check]]` with
`nightly_only = true`, its command and suite registration. Do not mix its
commands into a default functional check. The nightly workflow's manual
trigger runs the full nightly set; local xtask commands can still run any
individual scenario directly.

## Execution And Results

A preparation job builds and uploads `tg-xtask` for artifact-consuming checks.
Tests reuse `reusable-check-matrix.yml` and the existing runner profiles.
Matrix fail-fast is disabled. The final job reports the tested SHA and stage
results in the GitHub job summary, and fails if any required stage did not
succeed. Detailed output remains in each matrix job's Actions log.

Checks marked `performance_report = true` (currently the OrangePi vCPU
throughput and AXIVC Zephyr-Starry benchmark board tests) additionally get
their result lines extracted into the job summary and the workflow summary:
the runner captures the command log, `scripts/test/ci_perf_report.py` renders
`VCPU_PERF_RESULT` and `AXVISOR_IVC_BENCH_RESULT=` lines as a Markdown table,
each matrix job appends its table to its own summary, and the final job
merges the uploaded per-check report artifacts under a "Performance Results"
section (retained 30 days). Reports render only when the check succeeds; a
failed run still exposes its numbers through the matrix job log.

Nightly runs do not cancel one another. Board availability, reservation and
reset remain the responsibility of the existing board test service, shared
with PR CI. Scheduling after Starry Apps reduces overlap but does not provide
cross-workflow board exclusion by itself.

Existing image/rootfs requirements still apply. In particular, the OrangePi
IVC test requires the matching tgosimages IVC payload in the board Linux rootfs.
This first version does not add automated rootfs provisioning, cross-run
performance trending, long-duration stress tests or extra log artifacts.

## Local Planning

Generate the exact nightly matrix without starting QEMU or reserving boards:

```sh
python3 scripts/test/ci_plan.py --mode axvisor-nightly \
  --repository rcore-os/tgoskits --repository-owner rcore-os \
  --event-name schedule
python3 -m unittest discover -s scripts/test -p 'test_ci*.py'
```

Existing commands remain available for local test execution, for example:

```sh
cargo xtask axvisor test qemu --arch aarch64 --test-case smoke
cargo xtask axvisor test board --board orangepi-5-plus-ivc-benchmark
```
