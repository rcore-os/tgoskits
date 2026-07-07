# APPLY - 把 monitor app 落入 StarryOS / tgoskits 测试树

## 目录布局（apps/starry/monitor/）

```
apps/starry/monitor/
├── prebuild.sh                              # 构建期 provision（下载 prometheus/node_exporter + apk add glances 闭包 + 解包 pyte/uvicorn wheel + 注入 carpet）
├── build-x86_64-unknown-none.toml           # 四架构内核 build feature
├── build-aarch64-unknown-none-softfloat.toml
├── build-riscv64gc-unknown-none-elf.toml
├── build-loongarch64-unknown-none-softfloat.toml   # LoongArch 必须 dynamic 平台（ax-driver/serial，不 opt-out）
├── qemu-x86_64.toml                         # 四架构 qemu 启动 + success/fail regex + timeout
├── qemu-aarch64.toml
├── qemu-riscv64.toml
├── qemu-loongarch64.toml                    # uefi=false / to_bin=true（dynamic 平台 raw boot）
├── assets/
│   ├── prometheus.yml                        # scrape 配置（node 作业 -> loopback :9100）
│   ├── build-loong-binaries.sh               # loong64 prometheus/node_exporter/grafana 的 Go 交叉编译 recipe
│   └── MANIFEST.md                           # 逐档 URL + sha256（prometheus/node_exporter/grafana/wheel）
├── programs/
│   ├── run-monitor.sh                        # on-target 启动器（装到 /usr/bin/run-monitor.sh）
│   ├── pty_tui_drive.py                      # PTY 驱动器（SS3 应用键模式）
│   └── pyte_assert.py                        # pyte 屏幕重建 + 三重不变量断言
└── python/
    ├── run_monitor.py                        # 编排器 + 唯一门控锚点（MONITOR_OK=8/8 + TEST PASSED）
    ├── PrometheusCarpet.py
    ├── GrafanaCarpet.py                      # headless grafana server + HTTP/API 断言
    ├── GlancesCliCarpet.py
    ├── GlancesHeadlessCarpet.py
    ├── GlancesTuiCarpet.py                   # pyte 真实断言的 curses TUI carpet（核心难点）
    ├── GlancesCsCarpet.py
    └── GlancesWebCarpet.py
```

## 运行

```bash
source <仓库根>/.starry-env.sh        # qemu-10
cargo xtask starry app qemu -t monitor --arch x86_64
cargo xtask starry app qemu -t monitor --arch aarch64
cargo xtask starry app qemu -t monitor --arch riscv64
cargo xtask starry app qemu -t monitor --arch loongarch64
```

判据：xtask `rc=0` + 日志 `SUCCESS PATTERN MATCHED` + `^MONITOR_OK=8/8` + `TEST PASSED`。

## prebuild 可调环境变量（均有安全默认）

- `MONITOR_APK_BRANCH`（默认 `v3.23`）、`ALPINE_CDN`、`MONITOR_APK_CACHE`（离线加速，miss 走网络非早退）。
- `MONITOR_ROOTFS_SIZE`（默认 `6G`；grow-only，overlay 经 debugfs 注入不 resize，欠大会静默截断大 .so/大二进制/grafana public）。
- `MONITOR_BINS_DIR`（可选）：预置可复现二进制目录，离线用；**loong64** 若已按 `assets/build-loong-binaries.sh`
  预编译成 `$MONITOR_BINS_DIR/{prometheus/<arch>/prometheus-3.11.3.linux-loong64.tar.gz, node_exporter/<arch>/node_exporter,
  grafana/<arch>/grafana-13.0.1.linux-loong64.tar.gz}`，prebuild 直接取用，免去构建期交叉编译。可再放
  `$MONITOR_BINS_DIR/grafana/grafana-13.0.1.db`（架构无关预迁移 SQLite 播种）。未预置时 prebuild 会尝试现场 Go
  交叉编译（**仅需 Go≥1.25，无需 Node**：prometheus 不带内嵌 web UI = 无 npm/yarn 前端构建，grafana v13 后端亦无需前端构建）。
- `MONITOR_GRAFANA_PREMIGRATE`（默认 `1`）：构建期尽力预迁移 grafana.db（native/qemu-user，有界，失败即回退 on-target 迁移）。
- `MONITOR_TUI_SETTLE` / `MONITOR_TUI_DWELL`（默认 6 / 2 秒）：TUI soak 首帧稳定 + 每步停留时长。慢架构可上调。
- `MONITOR_TUI_COLS` / `MONITOR_TUI_ROWS`（默认 200 / 50）：pty 尺寸。
- `MONITOR_GRAFANA_HEALTH_TRIES`（默认 `1200`）：grafana /api/health 轮询上限（1s 一次；给 TCG 首启迁移留足预算）。

## 关键约定

- **四架构均运行，无 SKIP**：prometheus/node_exporter/grafana 的 amd64/arm64/riscv64 走官方 release，loong64 从源码
  Go 交叉编译（grafana 仅后端 + 嫁接官方前端）；glances 四架构 apk 齐全。
- **LoongArch 必须 dynamic 平台**：`build-loongarch64*.toml` 保留 `ax-driver/serial` 且不 opt-out 动态平台，
  `qemu-loongarch64.toml` 用 `uefi=false / to_bin=true`（静态平台缺串口 TTY 绑定 → 早退）。
- **rootfs grow-only**：`prebuild.sh` 只增不减，never 吞掉 resize2fs 失败。
- **门控锚点唯一**：`MONITOR_OK=8/8` + `TEST PASSED` 只在 `run_monitor.py` 里输出（`run-monitor.sh` 从不输出），
  故 success_regex 不会被命令回显自匹配。
