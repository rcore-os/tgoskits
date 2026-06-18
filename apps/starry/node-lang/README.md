# Starry node-lang App — Node.js 22 language + stdlib carpet suite

This app runs an industrial, carpet-coverage **Node.js 22 (V8)** language + core
API test suite inside StarryOS QEMU, across `x86_64 / aarch64 / riscv64 /
loongarch64`. It validates the *language/runtime layer* (V8, the ES2023+ language
surface and the `node:*` core modules). The `node` CLI option surface is
separately validated as host-side auxiliary evidence. This app does NOT cover
third-party npm packages or native addons (vite/astro/npm/… are separate cases).

## Node.js 22

Node v22 (LTS) is musl-native in Alpine **v3.22 main** (no gcompat). `prebuild.sh`
provisions it portably (mirrors the merged `python-lang` app): it extracts the
base rootfs to a staging tree, `apk add nodejs icu-data-full` from Alpine v3.22
into it via `qemu-<arch>-static` (so it works for every target arch on an x86
build host), enforces a hard ≥22 version gate, then copies the `node` binary, its
shared-library closure (icu-libs/libssl3/libcrypto3/libstdc++/c-ares/brotli/
simdjson/…), and the ICU data table into the app overlay alongside the two carpet
sources under `/usr/bin`. If the build host has no network, the prebuild falls
back to a documented pre-fetched apk cache (`download/nodejs-apks/<arch>/`,
overridable via `NODE_APK_CACHE`) and installs the same closure offline.

> **`icu-data-full` is required.** Alpine's `icu-libs` ships only a data-less stub
> `libicudata`; the real 31.8 MB `icudt76l.dat` is in the separate
> `icu-data-full` package. Without it `Intl.NumberFormat` / `Intl.DateTimeFormat`
> / `Number.toLocaleString` throw `Icu error`. The carpet exercises `Intl`, so the
> data package is provisioned and the `/usr/share/icu` tree is copied into the
> overlay.

## Layout

```text
apps/starry/node-lang/
  prebuild.sh                  # install Node 22 + ICU data + stage carpets (overlay)
  build-<target>.toml          # StarryOS build config (4 targets)
  qemu-<arch>.toml             # QEMU run config (4 arches)
  node/
    run_node_carpet.sh         # on-target gate: runs node-carpet.js, prints TEST PASSED iff it printed NODE_CARPET_OK
    node-carpet.js             # language + stdlib carpet (the on-target gate; 359 checks on node v22.22.2 — a few are version-gated; prints NODE_CARPET_OK)
    node-cli-carpet.sh         # 184-check `node` CLI option-surface carpet — host-validated auxiliary, staged for inspection (NOT part of the on-target gate)
```

`node-carpet.js` covers the ES2023+ language surface (every primitive/operator,
typed arrays, Proxy/Reflect, generators/async, WeakRef/FinalizationRegistry,
Intl) plus the `node:*` core module index (fs/stream/crypto/events/util/assert/
net/http/dns/dgram/worker_threads/readline/timers/async_hooks/sqlite/…). Every
version-gated feature is guarded against the running Node major; pass and skip are
both logged. It prints `NODE_CARPET_OK` on its final line iff zero failures.

`node-cli-carpet.sh` exercises **every** documented `node` CLI option and
`NODE_*` env var (`node --help` / cli.html), each with an observable assertion or
an explicit, reasoned skip; v22-only flags are gated. It is **host-validated** and
staged into the rootfs for inspection, but is NOT part of the StarryOS on-target
gate: a few options (e.g. `--watch`) drive fork/exec of a child node + fs-watch
(inotify), which are known StarryOS process-model gaps, so its full surface is
validated on the host build machine rather than gated in QEMU.

## Run

```bash
cargo xtask starry app qemu -t node-lang --arch aarch64
cargo xtask starry app qemu -t node-lang --arch riscv64
cargo xtask starry app qemu -t node-lang --arch loongarch64
cargo xtask starry app qemu -t node-lang --arch x86_64
```

Success criterion: `run_node_carpet.sh` prints `TEST PASSED` on its final line iff
`node-carpet.js` printed `NODE_CARPET_OK` with zero failures
(`success_regex = (?m)^TEST PASSED\s*$`); any failure prints `TEST FAILED` (a
`fail_regex`, so the run fails fast).

> **Known risk — StarryOS V8 mmap pressure (#242).** Node/V8 reserves a large
> virtual-memory "cage" at startup and is sensitive to the kernel `mmap` policy;
> StarryOS issue #242 tracks V8 mmap pressure that has blocked heavier Node
> workloads (npm/vite/astro). This carpet uses **no native addons and no heavy JS
> graph**, so it is the lightest possible Node workload — but the on-target boot
> is still the real test of whether plain `node` starts and runs under StarryOS.
> The host stage (below) validates that the staged binary + closure are correct
> and the carpet is green under qemu-user; whether V8 initializes under the
> StarryOS kernel mmap path is verified by the on-target run.

> x86_64 requires OVMF/UEFI for StarryOS boot; the local app-qemu path
> (`-kernel`, no PVH note) cannot boot it. The current CI matrix skips
> `apps/starry/*` QEMU jobs by path filter (same as merged python-lang #1257),
> so x86_64 on-target evidence is not covered by CI. aarch64/riscv64/loongarch64
> boot locally via qemu-system-<arch>.
