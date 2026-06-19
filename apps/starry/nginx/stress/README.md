# Stress tests (placeholder)

No stress cases are wired up yet. `nginx-runner.sh stress` currently prints a
skip marker and exits successfully.

When stress cases are added, install their guest scripts via `prebuild.sh` and
extend `mode_stress` in `runner/nginx-runner.sh`.
