# monitor - 软件来源与可复现性（provenance）

适配目标：StarryOS × 四架构（x86_64 / aarch64 / riscv64 / loongarch64）。所有二进制/wheel/apk 均在
`prebuild.sh` 构建期抓取 + 校验，**无任何二进制入 git**。逐档 URL + sha256 见 `assets/MANIFEST.md`。

## 1. Prometheus 3.11.3 + promtool（github.com/prometheus/prometheus, v3.11.3 release）

- 资产：`prometheus-3.11.3.linux-<goarch>.tar.gz`，内含 `prometheus`（server）+ `promtool`，均为
  CGO-free 纯静态 Go 二进制（`file` 报 statically linked，`netgo,builtinassets` → web UI 已嵌），在
  musl/StarryOS 直接跑，无需 libc/ld-musl 接线。
- 官方 linux 架构含 amd64/arm64/**riscv64**（有 riscv64，**无 loong64**）。
- **x86_64 / aarch64 / riscv64**：官方预编译。sha256 与上游 `sha256sums.txt` 逐字一致（2026-07-02
  对 live release 复核）。
- **loongarch64**：上游不发 loong64 → 从 `v3.11.3` tag **Go 交叉编译**（`assets/build-loong-binaries.sh prometheus`）：
  `CGO_ENABLED=0 GOOS=linux GOARCH=loong64 go build -trimpath -tags netgo
   -ldflags "-X .../version.Version=3.11.3 …" ./cmd/{prometheus,promtool}`（需 Go≥1.25，无需 Node）。
  **无内嵌 web UI**：不带 `builtinassets` tag，故**不做** Node≥22 的 npm/yarn react-scripts 前端构建（该步重、脆、
  arch 无关，且 Node<22 会失败 `assets_embed.go: undefined EmbedFS`）。产物 API **全功能**（scrape / TSDB /
  PromQL / alerting 全在），仅缺内置 `/graph` HTML 页 - 而本 app 的 PROM carpet 只测 API/CLI（`--version` /
  `--help` / promtool / `/-/ready` / PromQL / scrape），且仪表盘网页由 grafana 提供，故不受影响。产物 `file`
  报 `LoongArch ELF, statically linked`，内嵌版本串 `3.11.3`。这是从官方源码交叉编译的产物，不是 SKIP。

## 2. node_exporter 1.11.1（github.com/prometheus/node_exporter, v1.11.1 release）

- 作用：prometheus 的真实抓取目标（暴露 `:9100/metrics`），使 scrape → ingest → query 主链真正跑通
  （`up{job="node"}==1`），而非只测合成 `vector(42)`。
- 单个纯 Go 静态二进制。官方 linux 架构含 amd64/arm64/**riscv64**（无 loong64）。
- x86_64 / aarch64 / riscv64：官方预编译（TARBALL sha256 与上游 `sha256sums.txt` 一致）。
- loongarch64：从 `v1.11.1` tag Go 交叉编译（一条命令，无前端；`assets/build-loong-binaries.sh node_exporter`）。

## 3. Grafana 13.0.1 OSS（dl.grafana.com）

- 资产：`grafana-13.0.1.linux-<goarch>.tar.gz`，内含单个 CGO-free 纯静态 Go 二进制 `bin/grafana`
  （`grafana server` / `grafana cli` 为子命令）+ `public/`（前端 SPA）+ `conf/`（defaults.ini）；内嵌
  SQLite 存储。在 musl/StarryOS 直接跑，无需 libc。
- 官方 linux 架构含 amd64/arm64/**riscv64**（有 riscv64，**无 loong64**）。
- **x86_64 / aarch64 / riscv64**：官方预编译（tarball sha256 与 dl.grafana.com `*.tar.gz.sha256` 一致，
  2026-07-03 对 live 复核）。
- **loongarch64**：上游不发 → grafana v13 后端**不嵌**前端（`embed.go` 仅嵌 cue.mod schema），故只需
  **Go 交叉编译后端** `bin/grafana`（无需 Node/yarn 建前端），再嫁接官方 riscv64 tar 的 `public/`/`conf/`
  （架构无关）。见 `assets/build-loong-binaries.sh grafana`。这是源码交叉编译的产物，不是 SKIP。
- prebuild 剥离 `public/**/*.map`（~290MB 浏览器调试件，server 端从不读），并**构建期尽力预迁移**架构无关的
  `grafana.db`（跳过 709 个首启 SQLite 迁移；失败则 on-target 现场迁移，carpet 两种情况都能过）。

## 4. glances 4.4.1 + 依赖闭包（Alpine v3.23, musl, 四架构）

- `apk add` 解析当前版本 + 目标架构 musl `.so` 全闭包（无钉死漂移 URL、无 cache-miss 早退）：
  - `glances 4.4.1-r1`（community）+ `py3-psutil 7.1.3-r0`（main，native `_psutil_linux.abi3.so` 每
    架构各自 musl so）。
  - web 形态闭包：`py3-fastapi` / `py3-starlette` / `py3-pydantic`（+ native `pydantic-core`）/
    `py3-anyio` / `py3-sniffio` / `py3-h11` / `py3-click` / `py3-jinja2`。
  - TUI：`py3-wcwidth`（pyte 唯一依赖）+ Alpine 自带 ncursesw/terminfo。
  - 以上四架构在 Alpine v3.23 均存在（已核对 APKINDEX）。
- `glances --version` 同时覆盖 glances（红线钉 4.4.1）+ psutil（版本柔性）两个版本号，API version 钉 4。

## 5. 纯 python wheel（Alpine v3.23 未收录，noarch）

- `pyte 0.8.2`、`uvicorn 0.34.0`：`py3-none-any` 纯 python wheel，按钉死 URL + sha256 取回校验后解包进
  site-packages。
  - pyte 运行时依赖仅 wcwidth（Alpine `py3-wcwidth`）。
  - uvicorn 运行时依赖 `click>=7` + `h11>=0.8`（Alpine `py3-click` / `py3-h11` 满足）；`standard` 额外项
    （httptools/uvloop/websockets）为可选且不使用 - glances 以默认 asyncio loop + h11 跑 uvicorn。

## 6. 依赖内核面

- prometheus/node_exporter/grafana：Go runtime（goroutine 调度 / GC / futex / getrandom / SIGURG 抢占 /
  epoll netpoll）、loopback TCP/HTTP、mmap-backed TSDB 写盘（prometheus）、内嵌 SQLite 存储 + 迁移
  （grafana，压 ext4/fsync 路径）、定时器/WAL。
- glances：psutil 重度读 `/proc/stat`、`/proc/meminfo`、`/proc/<pid>/*`；TUI 走 pty/tty（SS3 应用键模式、
  TIOCSWINSZ、备用屏）；web/cs 压 loopback socket（bind/listen/accept）。
