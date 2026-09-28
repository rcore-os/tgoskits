# AxVisor Nightly

The `AxVisor Nightly` workflow runs the registered AxVisor CI checks daily at
19:20 UTC (03:20 Beijing time). It also supports `workflow_dispatch`: trigger it
from the GitHub Actions page ("AxVisor Nightly" → "Run workflow", choose the
branch to test) or with `gh workflow run axvisor-nightly.yml --ref <branch>`.
The planner resolves the triggering commit once and every build and test checks
out that same SHA. Scheduled runs use the repository default branch; a manual
run uses the selected branch.

The workflow must be merged into the repository's default branch before the
scheduled run is available. Execution is limited to `rcore-os/tgoskits`, whose
self-hosted runners provide the required virtualization hosts and boards.

## Coverage

The source of truth is `.github/ci/checks/axvisor-nightly.toml` plus the
`group = "AxVisor"` checks in the shared `.github/ci/checks/benchmarks.toml`.
Nightly runs every enabled check in those manifests without changed-file
filtering, including:

- AArch64 GICv2/GICv3 timer stress and the OrangePi Linux PCI network ping.
- The OrangePi virtio-net peer scenario.
- The OrangePi Zephyr-Starry IVC benchmark, single ArceOS guest performance and
  task switch benchmark from `benchmarks.toml`.

Only these manifests feed the nightly matrix; the default `axvisor.toml`
checks do not run at night. This is not a claim that every AxVisor feature or
every test-suit directory has a corresponding nightly test. Add new scenarios
to the existing test-suit and register them in `axvisor-nightly.toml` (or the
`group = "AxVisor"` section of `benchmarks.toml` for performance cases); do not
duplicate their shell commands in the nightly workflow.

## Default CI Versus Nightly

Default CI runs the functional `axvisor.toml` checks. The GICv2/GICv3 timer
stress, OrangePi Linux PCI network ping, OrangePi virtio-net peer, Zephyr-Starry
IVC benchmark, single ArceOS guest performance and task switch benchmark were
split into `axvisor-nightly.toml` and the `group = "AxVisor"` checks in the
shared `benchmarks.toml`. The manifest file name is the only authority for the
automatic nightly and performance report semantics, so individual checks no
longer declare `nightly_only` or `performance_report`. Ordinary CI excludes
these manifests for PRs, pushes and manual runs, even if the change precisely
selects their test-suit files; a PR changing only nightly test scenarios still
receives static checks. The default OrangePi Linux check explicitly selects only
the `smoke` case.

To add another nightly scenario, register its own `[[check]]` in
`axvisor-nightly.toml` with its command and suite registration; performance
measurements go to the `group = "AxVisor"` checks in `benchmarks.toml`. Do not
mix their commands into a default functional check. The nightly workflow's
manual trigger runs the full nightly set; local xtask commands can still run any
individual scenario directly.

## Execution And Results

A preparation job builds and uploads `tg-xtask` for artifact-consuming checks.
Tests reuse `reusable-check-matrix.yml` and the existing runner profiles.
Matrix fail-fast is disabled. The final job reports the tested SHA and stage
results in the GitHub job summary, and fails if any required stage did not
succeed. Detailed output remains in each matrix job's Actions log.

Every `group = "AxVisor"` check from `benchmarks.toml` (currently the OrangePi
vCPU throughput and AXIVC Zephyr-Starry benchmark board tests, plus the task
switch benchmark) additionally gets its result lines extracted into the job
summary and the workflow summary: the runner captures the command log,
`scripts/test/ci_perf_report.py` renders `VCPU_PERF_RESULT` and
`AXVISOR_IVC_BENCH_RESULT=` lines as a Markdown table, each matrix job appends
its table to its own summary, and the final job merges the uploaded per-check
report artifacts under a "Performance Results" section (retained 30 days).
Reports render only when the check succeeds; a failed run still exposes its
numbers through the matrix job log.

A final `Performance History` job also collects the per-check benchmark JSON,
appends it to the `perf-data` branch, and renders a Chart.js dashboard
(`scripts/test/ci_perf_dashboard.py`). Each test case gets its own chart (vCPU
throughput, IVC send, IVC receive), the x-axis is the nightly date, lines are
unfilled, and charts show the most recent 7 nightly entries while
`history.json` keeps all of them. The job then dispatches `docs.yml`, which
merges `perf-data` into `docs/build/axvisor-perf` before publishing Pages, so
the dashboard appears next to the documentation at
`<docs-site>/axvisor-perf/`. `docs.yml` also rebuilds nightly as a fallback.
Only runs of `dev` publish this shared history, so manually testing another
branch cannot add experimental measurements to the dashboard. The job never
touches the Pages deployment itself and writes history only in
`rcore-os/tgoskits`.

Nightly runs do not cancel one another. Board availability, reservation and
reset remain the responsibility of the existing board test service, shared
with PR CI. Scheduling after Starry Apps reduces overlap but does not provide
cross-workflow board exclusion by itself.

Existing image/rootfs requirements still apply. In particular, the OrangePi
IVC test requires the matching tgosimages Zephyr image and Starry userspace
benchmark in the board Linux rootfs. Its Starry kernel is embedded from the
current checkout by the board check after `cargo xtask starry build`; the
preinstalled `/guest/starry` kernel is not used. This first version does not add
automated rootfs provisioning, regression thresholds or extra log artifacts.

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
