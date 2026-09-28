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

The source of truth is `.github/ci/checks/axvisor-nightly.toml`. Nightly runs
every enabled check in that manifest without changed-file filtering, including:

- AArch64 GICv2/GICv3 timer stress and the OrangePi Linux PCI network ping.
- The OrangePi virtio-net peer scenario.

Only that manifest feeds the nightly matrix; the default `axvisor.toml` checks
do not run at night, and performance measurements are not part of this
workflow. Performance checks live in `.github/ci/checks/benchmarks.toml` and
run through `.github/workflows/benchmarks.yml`. This is not a claim that every
AxVisor feature or every test-suit directory has a corresponding nightly test.
Add new functional scenarios to the existing test-suit and register them in
`axvisor-nightly.toml`; do not duplicate their shell commands in the nightly
workflow.

## Default CI Versus Nightly

Default CI runs the functional `axvisor.toml` checks. The GICv2/GICv3 timer
stress, OrangePi Linux PCI network ping and OrangePi virtio-net peer were split
into `axvisor-nightly.toml`. The manifest file name is the only authority for
automatic nightly semantics, so individual checks no longer declare
`nightly_only`. Ordinary CI excludes the nightly manifest for PRs, pushes and
manual runs, even if the change precisely selects its test-suit files; a PR
changing only nightly test scenarios still receives static checks. The default
OrangePi Linux check explicitly selects only the `smoke` case.

To add another nightly scenario, register its own `[[check]]` in
`axvisor-nightly.toml` with its command and suite registration. Do not mix
nightly or performance commands into a default functional check. Performance
measurements belong to the `group = "AxVisor"` checks in `benchmarks.toml` and
are executed by the separate benchmarks workflow. The nightly workflow's manual
trigger runs the full nightly set; local xtask commands can still run any
individual scenario directly.

## Execution And Results

Tests reuse `reusable-check-matrix.yml` and the existing runner profiles. The
nightly workflow does not prepare a `tg-xtask` artifact; each row runs its
declared `cargo xtask` command directly. Matrix fail-fast is disabled. The final
job reports the tested SHA and plan/check results in the GitHub job summary, and
fails if either stage did not succeed. Detailed output remains in each matrix
job's Actions log.

Performance reports are produced by `.github/workflows/benchmarks.yml`. Its
`benchmark-updates` job hands this run's AxVisor and Starry increments to the
docs workflow as a short-lived artifact, and the docs Pages deployment merges
them with the published benchmark history. If both Pages files are initially
missing, docs performs a one-time read-only bootstrap from the frozen legacy
`perf-data` branch and blocks deployment unless both legacy files are readable;
after that publication, Pages is the only persistent history source.
See the [CI performance
benchmarks](../../docs/docs/ci/testing/benchmarks.md) for report prefixes,
dashboard sources and history publishing. This page only covers AxVisor's
functional nightly.

Nightly runs do not cancel one another. Board availability, reservation and
reset remain the responsibility of the existing board test service, shared
with PR CI. Scheduling after Starry Apps reduces overlap but does not provide
cross-workflow board exclusion by itself.

## Local Planning

Generate the exact nightly matrix without starting QEMU or reserving boards:

```sh
python3 scripts/test/ci_plan.py --mode axvisor-nightly \
  --repository rcore-os/tgoskits --repository-owner rcore-os \
  --event-name schedule
python3 scripts/test/ci_plan.py --mode benchmarks \
  --repository rcore-os/tgoskits --repository-owner rcore-os \
  --event-name schedule
python3 -m unittest discover -s scripts/test -p 'test_ci*.py'
```

Existing commands remain available for local test execution, for example:

```sh
cargo xtask axvisor test qemu --arch aarch64 --test-case smoke
cargo xtask axvisor test board --board orangepi-5-plus-virtio-net-peer
```
