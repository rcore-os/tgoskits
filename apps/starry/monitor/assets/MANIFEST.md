# monitor app -- pinned binary/wheel provenance (URLs + sha256)

All artifacts are fetched at build time by `prebuild.sh`; NOTHING binary is committed. amd64/arm64/
riscv64 prometheus + node_exporter sha256 are byte-for-byte identical to the upstream release
`sha256sums.txt` (re-verified against the live release on 2026-07-02). loong64 has no upstream
prebuilt -> Go-cross-compiled from the pinned source tag (`assets/build-loong-binaries.sh`), which
is non-deterministic; the recorded sha is of the reference reproducible artifact.

## prometheus 3.11.3 (github.com/prometheus/prometheus)
Release asset: `prometheus-3.11.3.linux-<goarch>.tar.gz` (contains `prometheus` + `promtool`,
static CGO-free Go, netgo,builtinassets => embedded UI). Upstream linux arches include amd64/arm64/
riscv64 (has riscv64, NO loong64).

    URL base: https://github.com/prometheus/prometheus/releases/download/v3.11.3/prometheus-3.11.3.linux-{amd64,arm64,riscv64}.tar.gz

| arch        | goarch  | sha256 (tarball)                                                   | source            |
|-------------|---------|--------------------------------------------------------------------|-------------------|
| x86_64      | amd64   | 9479af67673316278958cda1f39b88a09f8921084e039c65acca060d0447bb38   | official prebuilt |
| aarch64     | arm64   | d2ec0a96259afde955ad1560ced303cef99cac4dac676bd4dd7614d76adb708a   | official prebuilt |
| riscv64     | riscv64 | bd6978937d64f4afa82919e0c4b3b83ace50808b953ab6174e480ca7dda2ba9a   | official prebuilt |
| loongarch64 | loong64 | ca42f362268d3e404deeec608aa23faadc06f1c0ad9f0036c8650b6a12a69d73   | Go cross-compiled (go1.26.3, tag v3.11.3, **no embedded web UI**; non-deterministic) |

loong64 prometheus/promtool are built WITHOUT the embedded `/graph` web UI (no `builtinassets` tag ->
no Node/npm frontend build; just `go build ./cmd/{prometheus,promtool}`, Go>=1.25). The binary keeps
full API/server functionality (scrape / TSDB / PromQL / alerting); only the built-in HTML dashboard is
absent, which the PROM carpet does not test (it tests the API/CLI) and which grafana provides in this
stack. Verified: `file` -> `LoongArch ELF, statically linked`; under qemu-loongarch64: `prometheus
--version` -> `prometheus, version 3.11.3 ... platform: linux/loong64`, promtool likewise 3.11.3.

## node_exporter 1.11.1 (github.com/prometheus/node_exporter)
Release asset: `node_exporter-1.11.1.linux-<goarch>.tar.gz` (single static Go binary). Upstream
linux arches include amd64/arm64/riscv64 (has riscv64, NO loong64).

    URL base: https://github.com/prometheus/node_exporter/releases/download/v1.11.1/node_exporter-1.11.1.linux-{amd64,arm64,riscv64}.tar.gz

TARBALL sha256 (upstream sha256sums.txt):

| arch        | goarch  | sha256 (tarball)                                                   | binary sha256 (extracted)                                         | source            |
|-------------|---------|--------------------------------------------------------------------|------------------------------------------------------------------|-------------------|
| x86_64      | amd64   | 9f5ea48e5bc7b656f8a91a32e7d7deb89f70f73dabd0d974418aca15f37d6810   | 3a01a3cc7f69798698fbb31b24e5ee279dd2c39727be2a5a65071536fc16b455 | official prebuilt |
| aarch64     | arm64   | ba1886efbd76cb96b0087c695ea8d1b9cb6e8aa946c996d744e9ee16c8e3591a   | c92bd1e9eeb4061f1bdbf60a7b41d446220a4a44d87fb77ae889f034cb8cf3bc | official prebuilt |
| riscv64     | riscv64 | 8d73447c47488a94f7eba467838c815ea7dceb449c75b1b8e91fa6dc3e0e364e   | 3932dd6b4456eda301f3198fe0c22860b66afbe5ffa8214a32df532e490e5d21 | official prebuilt |
| loongarch64 | loong64 | (Go cross-compiled; no tarball)                                    | 76c56223816d403761564b581e8961d59360e0d50f7343d50a60767630306eba | Go cross-compiled (tag v1.11.1) |

## grafana 13.0.1 OSS (dl.grafana.com)
Release asset: `grafana-13.0.1.linux-<goarch>.tar.gz` (single static CGO-free Go `bin/grafana` +
`public/` frontend SPA + `conf/`; embedded SQLite store). Upstream linux arches include amd64/arm64/
riscv64 (has riscv64, NO loong64). Tarball sha256 == dl.grafana.com `*.tar.gz.sha256` (re-verified live 2026-07-03).

    URL base: https://dl.grafana.com/oss/release/grafana-13.0.1.linux-{amd64,arm64,riscv64}.tar.gz

| arch        | goarch  | sha256 (tarball)                                                   | source            |
|-------------|---------|--------------------------------------------------------------------|-------------------|
| x86_64      | amd64   | 187ddc4badb69aecb7cd3fae2884add7ed21adde7124a6f8093b7b4033d722f2   | official prebuilt |
| aarch64     | arm64   | 553d5ee3fb1600c83ef2fbf336579ed6cc64fffc328843ea7d662f85b876c261   | official prebuilt |
| riscv64     | riscv64 | 233ac9bf87390f203e45a1beb47630b28d3eb0c0dce3bfc5838e0e1603eb2cee   | official prebuilt |
| loongarch64 | loong64 | a3f14993f0fce3419ded61f41f8ab0695181a822501a783168e37993bcac660f   | Go cross-compiled BACKEND (tag v13.0.1) + official riscv64 tar public/ grafted; non-deterministic |

grafana v13's backend does NOT embed the frontend, so loong64 needs only the Go backend cross-compiled
(no Node/yarn build) with the official arch-independent frontend grafted -- `assets/build-loong-binaries.sh grafana`.
prebuild strips `public/**/*.map` (~290MB browser debug artifacts, never read server-side) and
best-effort pre-migrates the arch-independent `grafana.db` (skips the 709 first-run SQLite migrations).

## glances 4.4.1 stack (Alpine v3.23, musl, all 4 arches -- resolved live by `apk add`)
No pinned URLs: `apk add` resolves the CURRENT version + full musl .so closure per arch (no drift).

    glances 4.4.1-r1 (community) + py3-psutil 7.1.3-r0 (main, native _psutil_linux.abi3.so per arch)
    py3-fastapi 0.121.2 / py3-starlette 0.47.2 / py3-pydantic 2.12.3 (+pydantic-core native) /
    py3-anyio 4.11.0 / py3-sniffio 1.3.1 / py3-h11 0.16.0 / py3-click 8.1.8 / py3-wcwidth 0.2.13 /
    py3-jinja2  -- all present for x86_64/aarch64/riscv64/loongarch64 in Alpine v3.23.

## vendored pure-python wheels (py3-none-any; NOT in Alpine v3.23)
| package        | version | sha256                                                           | url |
|----------------|---------|------------------------------------------------------------------|-----|
| pyte           | 0.8.2   | 85db42a35798a5aafa96ac4d8da78b090b2c933248819157fc0e6f78876a0135 | https://files.pythonhosted.org/packages/59/d0/bb522283b90853afbf506cd5b71c650cf708829914efd0003d615cf426cd/pyte-0.8.2-py3-none-any.whl |
| uvicorn        | 0.34.0  | 023dc038422502fa28a09c7a30bf2b6991512da7dcdb8fd35fe57cfc154126f4 | https://files.pythonhosted.org/packages/61/14/33a3a1352cfa71812a3a21e8c9bfb83f60b0011f5e36f2b1399d51928209/uvicorn-0.34.0-py3-none-any.whl |

pyte's only runtime dep is wcwidth (Alpine py3-wcwidth). uvicorn's runtime deps click>=7 + h11>=0.8
are satisfied by Alpine py3-click / py3-h11 (the "standard" extras httptools/uvloop/websockets are
optional and unused -- glances runs uvicorn with the default asyncio loop + h11).
