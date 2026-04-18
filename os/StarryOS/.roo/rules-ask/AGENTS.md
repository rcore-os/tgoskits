# Ask Mode Rules — StarryOS

- `arceos/` is a git submodule EXCLUDED from the workspace — ax-* crates are consumed from crates.io (v0.5.0), not as path dependencies. Changes to ax-* crates require publishing to crates.io first.
- Crate dependency chain: `starryos` (binary) → `kernel` (`starry-kernel` lib) → `ax-*` crates from crates.io. External `starry-*` crates (starry-process, starry-signal, starry-vm) also on crates.io.
- `xtask` binary has dual compilation: full build tool on host (clap+tokio+axbuild deps), stub panic loop on target — don't assume xtask code runs in-kernel.
- Config system merges 3 layers: `make/defconfig.toml` + platform config + `EXTRA_CONFIG` env var → `.axconfig.toml`. All three can affect behavior.
- `CONTRIBUTING.md` requires Conventional Commits format: `type(scope): subject`.
- Supported architectures: riscv64 (default), aarch64, loongarch64, x86_64 (WIP). Architecture affects syscall surface and memory model.
- `scripts/test.sh` runs 5 steps (tool check, fmt, per-arch build+boot, publish dry-run, summary); run individual steps: `./scripts/test.sh 3`.
