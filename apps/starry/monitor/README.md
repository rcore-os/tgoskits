# monitor - Prometheus 监控栈 + Grafana + glances 系统监控器（StarryOS 四架构地毯测试）

本 app 在 StarryOS 四架构（x86_64 / aarch64 / riscv64 / loongarch64）单核 qemu 上，对三大监控软件做工业级、端到端、逐断言的地毯测试（非 “进程能起来” 的冒烟）：

- **Prometheus**（CNCF 监控系统：拉取式指标抓取 + TSDB + PromQL 引擎；`prometheus` 3.11.3 + `promtool`，CGO-free 纯静态 Go 二进制）＋ **node_exporter** 1.11.1（最简 exporter，作为真实抓取目标）。
- **Grafana**（观测/可视化 web 应用，13.0.1；单个 CGO-free 纯静态 Go 二进制 + 内嵌前端 SPA + 内嵌 SQLite） - headless 起 server + HTTP 断言（无浏览器、无 TUI）。
- **glances**（基于 psutil 的 Python 系统监控器，4.4.1）的全部五种运行形态：CLI / headless / **TUI（pyte 真实断言）** / client-server / web。

单次启动运行 8 个子 carpet，全过才由 `run_monitor.py` 输出唯一门控锚点 `MONITOR_OK=8/8` + `TEST PASSED`。

## 测试维度

### Prometheus（`PrometheusCarpet.py`）
1. **版本红线**：`prometheus --version` 与 `promtool --version` 必须精确报 `3.11.3`；`node_exporter --version` 报 `1.11.1`。版本不符即判测例无效。
2. **`--help` 全树**：`prometheus --help` / `--help-long` 的旗标树 + `promtool --help` 子命令树（check / query / debug / test / tsdb / push，及 check 的 config/rules/metrics）逐项断言存在。
3. **promtool 校验**：`promtool check config /etc/prometheus.yml` 返回 SUCCESS；`promtool query instant` 作为 PromQL 客户端向在跑的 server 发查询。
4. **promtool tsdb 功能腿**：`tsdb create-blocks-from openmetrics`（从 OpenMetrics 造块）→ `tsdb list`（打印块 ULID 验创建成功）→ `tsdb analyze`（读块统计）。全在 `/root`（ext4 磁盘）而非 `/tmp`：TSDB 与临时目录落磁盘既是生产正确做法，也规避 starry tmpfs 的 mmap read-back 返零页问题（analyze 经 mmap 读块 meta/index 会得 `\x00`；`tsdb list` 走 `read()` 不受影响）。此 tmpfs mmap 缺陷单列内核跟进。
5. **ready**：headless 拉起 prometheus server 于 loopback `:9090`，断言日志 `Server is ready to receive web requests.` 且 `/-/ready` 返回 `Ready`（HTTP 服务起来 + TSDB 打开）。
6. **PromQL 引擎**：`/api/v1/query?query=vector(42)` 返回 `status:success` + 值 `42`。
7. **scrape + 端到端集成测**：前台拉起 node_exporter 于 `:9100/metrics`（启 `--collector.uname/cpu/meminfo/loadavg/netdev/diskstats/filesystem`，覆盖 starry 现渲染的全部 procfs 源）作为真实抓取目标，prometheus 配 `prometheus.yml` scrape job `node`。先断言 `/api/v1/query?query=up{job="node"}` 返回 `1`（真抓到 + 入库），再经 **180s soak**（2s 抓取间隔多轮）后断言 `node_cpu_seconds_total`、`node_memory_MemTotal_bytes` 与 `node_network_receive_bytes_total`（netdev collector 产）均已以数值样本入 TSDB、且 `query_range` 在时窗内返回多个数据点。这是 scrape→store→query 全链路的工业级端到端验证：node_exporter 真读 starry `/proc/stat`·`/proc/meminfo`·`/proc/loadavg`·`/proc/net/dev`·`/proc/diskstats`·`/proc/mounts` 暴露指标，prometheus 真抓真存真查。

验证内核面：Go runtime（M:N 调度 / GC / futex 停泊 / getrandom）、loopback TCP/HTTP 双向、磁盘（ext4）TSDB 块写读 + mmap（analyze）、定时器 / WAL、procfs（`/proc/stat`·`/proc/meminfo`·`/proc/loadavg`·`/proc/net/dev`·`/proc/diskstats`·`/proc/mounts` 喂 node_exporter collector）。

### Grafana（`GrafanaCarpet.py`）
Grafana 是单个纯静态 Go 二进制（`grafana server` 子命令），serve 内嵌前端 SPA + REST API 于 loopback `:3000`，后端为内嵌 SQLite - 故像 glances web / prometheus 一样 **headless 起 server + HTTP 断言**，不测 TUI。
1. **版本红线**：`grafana --version` 必须精确报 `13.0.1`。
2. **启动**：`grafana server --homepath <dir> --config <min.ini>` 起在 loopback，断言启动日志出现 `HTTP Server Listen` 与 `migrations completed`（server 起 + SQLite 迁移完成）。
3. **health**：`GET /api/health` 返回 `200` + JSON `{"database":"ok","version":"13.0.1"}`（SQLite 后端就绪）。
4. **frontend**：`GET /login` 返回 `200`，serve 内嵌 Grafana SPA（`<title>Grafana</title>` + `grafanaBootData`）。
5. **API/静态**：`GET /`（根）到达 app（200/30x → /login），`GET /robots.txt` 从内嵌 `public/` 静态 serve `200`。

确定性：固定 homepath/config + 每次运行独立临时 data 目录（从构建期预迁移的 `grafana.db` 播种，跳过 709 个首启迁移），无外部 datasource，无网。验证内核面：Go runtime、loopback HTTP、内嵌 SQLite 存储（ext4/fsync 路径）。

### glances（5 个 carpet）
- **CLI**（`GlancesCliCarpet.py`）：`--version` 版本红线（glances 4.4.1 / API 4 / PsUtil 版本行）＋ `--help` 全选项树逐项断言（每个文档化选项、五种运行模式 `-c/-s/-w/--browser/--stdout`、`--sort-processes` 全部取值、默认端口 61209）＋ `--modules-list` ＋ `--print-completion`。
- **headless**（`GlancesHeadlessCarpet.py`）：`--stdout cpu,mem,load` 单帧快照，断言 psutil 真读到 `/proc` 的数（`mem.total > 1 MiB`、cpu 规范字段、load `min1`）＋ `--stdout-json` 机器可读路径。
- **TUI（pyte 真实断言，核心难点）**（`GlancesTuiCarpet.py`）：见下节。
- **client-server**（`GlancesCsCarpet.py`）：`glances -s`（XML-RPC server）＋ `glances -c`（client）经 loopback 拉取快照，断言 client 从 server 拿到合理 `mem.total`（c/s 往返真通）。
- **web**（`GlancesWebCarpet.py`）：`glances -w`（FastAPI + uvicorn）起 ASGI server，断言 REST API `/api/4/status`、`/api/4/all`（完整 JSON 快照）、`/api/4/mem/total`、`/api/4/cpu` 的 HTTP + JSON 往返。

## TUI pyte 方法学（`GlancesTuiCarpet.py` + `programs/pty_tui_drive.py` + `programs/pyte_assert.py`）

glances 是全屏 curses 程序。本 carpet 用**离线终端仿真（pyte）**对其做真实断言，而不是 “进程起来了” 的冒烟：

1. **真 PTY 驱动**：`pty.fork` 在真伪终端里跑 glances（走 initscr → smkx → 备用屏的生产渲染路径），用 `TIOCSWINSZ` 设窗口尺寸，持续抽取它写回的原始 ANSI 字节流。
2. **SS3 方向键（关键事实）**：glances 调 `keypad(True)` → 进入应用光标键模式（DECCKM），它期望的方向键是 **SS3 形式 `ESC O A/B/C/D`**，不是 CSI 形式 `ESC [ A`。若发 CSI，ncurses 把落单的 ESC 当 “退出” → glances 立刻退出。因此驱动器发 SS3 方向键；PgUp/PgDn/功能键用各自的 terminfo 序列。**“glances 熬过整个方向键 soak 没退出” 本身就是对 StarryOS pty 正确投递 SS3 的硬断言**。
3. **交互中显示**：发排序热键 `m` / `c` / `a`，断言屏上 “Threads sorted …” 状态行相应变为 memory / CPU / automatic（交互真落地且屏幕连贯重绘）。
4. **pyte 三重断言**（对捕获流做离线重建）：
   - **golden 静态骨架（core 硬断言）**：进程表表头（CPU% / MEM% / PID / USER / TIME+ / Command）＋核心区块 TASKS / MEM / LOAD 确实被画出。这些读的是 StarryOS 稳定提供的核心 /proc（`/proc/[pid]/stat`、`/proc/stat`、`/proc/meminfo`、`/proc/loadavg` - headless carpet 已在 on-target 证明）。
   - **左侧栏 procfs 区块（硬断言）**：NETWORK（`/proc/net/dev`）/ DISK I/O（`/proc/diskstats`）/ FILE SYS（`/proc/mounts`+statfs）。StarryOS 现真实渲染这三个 `/proc` 源（procfs 已补齐真数据），glances 仅当插件有非空 stats 时才画出该区块，故重建屏幕里出现这三个区块标签本身就是插件读到真数据的证据：NETWORK 带回环接口、DISK I/O 带根 virtio-blk 盘 `vda`、FILE SYS 带 ext4 根挂载。三区块作**硬门控**断言（非 best-effort），并额外断言区块内出现具体数据 token（`vda` 在 DISK I/O 与 FILE SYS 根行 `/ (vda)`；`eth0` 在 NETWORK）。headless carpet 对同样三区块做数值级硬断言（`lo` 累计字节 >0 / `vda` 累计 read_bytes >0 / 根 fs size >0）。
   - **跨帧稳定性**：稳定锚点用**始终渲染的 core 进程表表头**，每帧落在**同一行同一列** - 无闪烁 / 无残影 / 无错位刷新（左侧栏行的数值随刷新变化，故不作定位锚点）。
   - **差分渲染不变量**：把字节流按任意分块边界喂 pyte，得到的屏幕矩阵与整块一次性喂**逐格完全一致** - 任何转义序列被切断 / 丢失 / 损坏都会被抓出。
5. **大区域重绘**：`-2`（左侧栏）/ `-3`（quicklook）开关来回，断言 **core 骨架**重绘回来、无残影。
6. **内容驱动捕获（跨 arch 时序鲁棒）**：每次交互（启动 / 发 `m`/`c`/`a`/方向键 / 开关）后，驱动器持续把 PTY 输出增量喂给一块 live pyte 屏，**轮询等到预期内容真渲染出来再捕获断言帧**（启动等进程表头 `CPU%`/`MEM%`/`PID`；排序热键等排序指示行真出现 `by memory`/`by CPU`/`automatically`），设慷慨的 per-arch 上限（settle 90s、每步 45s，`MONITOR_TUI_SETTLE_WAIT`/`MONITOR_TUI_STEP_WAIT` 可调）+ 每步最小停留（保持真 soak）。快 arch（x86）立即满足即捕获，慢 arch（aa/rv/loong TCG）会等到渲染完成 - **捕获时机由内容驱动而非固定延时**，故同一脚本在四架构都稳；到上限仍无预期内容则**据实失败**（捕获帧缺内容，下游断言真报错，绝不静默放过）。

> **glances CLI 版本感知说明**：`--print-completion`（shell 补全）由可选依赖 `shtab` 提供；交付版 Alpine glances 4.4.1-r1 的 apk 闭包**不含 shtab**，故该选项本就不提供 - carpet 对它做**能力探测**（存在则测其产出补全脚本，缺失则据实记为 documented SKIP），不假设 host 版本。其余全部 `--help` 选项按**交付版 4.4.1-r1 实测选项全集**逐项硬断言。

## 四架构来源（provenance，据实注明）

| 组件 | 版本 | x86_64 | aarch64 | riscv64 | loongarch64 |
|------|------|--------|---------|---------|-------------|
| prometheus + promtool | 3.11.3 | 官方预编译 | 官方预编译 | **官方预编译**（上游发 riscv64）| **Go 交叉编译**（上游无 loong64，从 v3.11.3 tag，**无内嵌 web UI**：API 全功能 scrape/PromQL/alert 都在，仅无内置 /graph 网页 - PROM carpet 只测 API，网页本由 grafana 提供，故不影响）|
| node_exporter | 1.11.1 | 官方预编译 | 官方预编译 | 官方预编译 | **Go 交叉编译**（从 v1.11.1 tag，无前端）|
| grafana | 13.0.1 | 官方预编译 | 官方预编译 | **官方预编译**（上游发 riscv64）| **Go 交叉编译后端** + 嫁接官方前端（上游无 loong64；v13 后端不嵌前端，只交叉编译 `bin/grafana`，无需 Node）|
| glances + psutil + FastAPI/uvicorn 栈 | 4.4.1 | apk | apk | apk | apk |

- prometheus / node_exporter / grafana 为 CGO-free 纯静态 Go 二进制，在 musl/StarryOS 直接跑，无需 libc；四架构均运行，无任何 SKIP。amd64/arm64/riscv64 走官方 release（prometheus/node_exporter 的 URL + sha256 与上游 `sha256sums.txt` 逐字一致；grafana 与 dl.grafana.com `*.tar.gz.sha256` 一致），loong64 从源码 Go 交叉编译（`assets/build-loong-binaries.sh`）。逐档 URL/sha256 见 `assets/MANIFEST.md`。
- glances 及其闭包（psutil native、FastAPI/Starlette/pydantic、jinja2）由 `apk add` 解析当前版本 + 目标架构 musl `.so` 全闭包，四架构均有；Alpine v3.23 未收录的 `pyte` / `uvicorn` 为纯 python（noarch）wheel，按钉死 URL + sha256 取回校验后解包进 site-packages。
- **无任何二进制入 git**；全部构建期抓取 + 校验（见 `prebuild.sh`）。

## 运行

```bash
# 1) 构建 overlay（下载 prometheus/node_exporter/grafana + apk add glances 闭包 + 解包 pyte/uvicorn wheel + grafana.db 预迁移 + 注入 carpet）
#    由 app 运行框架自动调用 prebuild.sh；四架构分别构建。
# 2) 运行（单核）
source <仓库根>/.starry-env.sh          # 使用 qemu-10
cargo xtask starry app qemu -t monitor --arch <x86_64|aarch64|riscv64|loongarch64>
# 成功判据：rc=0 + 日志 SUCCESS PATTERN MATCHED + ^MONITOR_OK=8/8 + TEST PASSED
```

## 确定性与说明

- **网络/容器探测插件**：TUI/headless/web/cs carpet 均禁用会在离线 SLIRP guest 上阻塞首帧的环境探测插件（`ip` 公网 IP 查询、`cloud` 169.254 元数据、`containers`/`docker` socket、`ports`/`folders`/`connections`/`sensors` 扫描）。核心仪表盘（cpu/mem/load/quicklook/uptime/processlist）照常渲染，正是 golden 所硬断言的对象。golden 全为结构性（标签/列位），从不钉死宿主相关的数值。
- **左侧栏 procfs 区块（硬断言，据实真渲染）**：TUI/headless carpet 的 NETWORK/DISK I/O/FILE SYS 区块读 `/proc/net/dev`·`/proc/diskstats`·`/proc/mounts`+statfs。StarryOS 现真实渲染这三个源（`/proc/net/dev` 真 RX/TX 计数、`/proc/diskstats` 根盘 `vda` 真请求/扇区计数、`/proc/mounts` 真挂载表 + `statfs` 真容量），故三区块**硬门控**断言其真渲染 + 携带真数据：TUI 断言三区块标签 + `vda`/`eth0` 数据 token 出现，headless 断言 `lo` 累计字节 >0、`vda` 累计 read_bytes >0、根 fs size >0；node_exporter 同时启 netdev/diskstats/filesystem collector 并断言 `node_network_receive_bytes_total{lo}>0`、`node_disk_reads_completed_total{vda}>0`、`node_filesystem_size_bytes{"/"}>0`。core 区块（进程表 + MEM/LOAD/TASKS）一并硬断言。仍是长 soak + 三态 + 四不变量，非降级 smoke。
- **loopback**：grafana / glances web（uvicorn/ASGI）/ client-server（XML-RPC）均在 guest 内 loopback 上 bind/listen/accept，压 StarryOS 网络栈。
- **grafana SQLite 迁移**：grafana 首启要跑 709 个 SQLite 迁移（重压 ext4/fsync）。prebuild **构建期尽力预迁移**架构无关的 `grafana.db` 播种，on-target 跳过这 709 个迁移；carpet 用 WAL 模式 + 对前端端点做退避重试化解启动期 `SQLITE_BUSY` 争用。若预迁移未成，on-target 现场迁移（更慢但仍能过）。
- **超时**：本 app 单次启动跑 prometheus 冷启动 + scrape + grafana server（SQLite 迁移/启动）+ glances 五形态（含 TUI soak + web + c/s），故 `timeout` 取较大值（慢架构 TCG 下 Go/SQLite 冷启动需足够长，非真 hang）。
