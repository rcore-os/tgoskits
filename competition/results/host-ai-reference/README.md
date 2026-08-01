# Deterministic host reference

This directory is generated from the dependency-free thermal plant and the
checked-in `thermal-4x6x1-v1` model. It is a functional regression baseline,
not a substitute for the Linux-to-Zephyr end-to-end measurements.

Regenerate from the repository root with:

```text
cargo +nightly-2026-07-15 run -p ivcproto -- \
  evaluate-csv competition/results/host-ai-reference/raw.csv
```
