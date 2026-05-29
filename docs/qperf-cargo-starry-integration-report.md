# qperf 与 cargo starry 集成报告

## 1. 背景与目标

本轮目标是降低 qperf 使用门槛，并提升火焰图可读性：

* 用户不再需要手写复杂 `tools/starry-syscall-harness/harness.py perf-profile ...`。
* `cargo starry perf` 生成完整 qperf report、csv、folded stack、SVG flamegraph。
* `cargo starry run --perf` 作为 `cargo starry qemu --perf` alias 路径，提供 run 语义入口。
* 火焰图保留更完整的 Rust demangled symbol，支持 symbol style、focus、boot/workload/post/focused 视图。

本轮不修改 virtio 数据路径，也不宣称修复 virtio 性能问题。

## 2. 现有架构梳理

`.cargo/config.toml` 中 `cargo starry` 是 alias：`cargo run -p tg-xtask -- starry`。实际 StarryOS CLI 在 `scripts/axbuild/src/starry/mod.rs`，qperf runner 在 `scripts/axbuild/src/starry/perf.rs`。

本轮选择的集成点：

* 保留并增强已有 `Command::Perf(ArgsPerf)`，即 `cargo starry perf`。
* 给 `Command::Qemu(ArgsQemu)` 增加 `run` alias，并在 `ArgsQemu` 上增加 `--perf` 与 `--perf-*` 参数。
* 继续复用 `perf::run()`，避免复制 qperf/QEMU/plugin/analyzer 逻辑。
* 增加 harness `perf-postprocess`，让 cargo 入口也能生成与 harness 一致的 `report.json/report.md/hotspots.csv/hotspot_categories.csv`。
* `cargo starry perf` 默认把 axbuild 临时目录隔离到输出目录下的 `axbuild-tmp/`；底层 `axbuild_tmp_dir()` 也支持 `AXBUILD_TMP_DIR`，避免已有 `tmp/axbuild/` 被 Docker/root-owned 文件污染时阻塞 profile。

## 3. 新命令设计

### `cargo starry perf`

```bash
cargo starry perf --case boot
```

默认值：

| 参数 | 默认值 |
| --- | --- |
| arch | `riscv64` |
| case | `boot` |
| output | `target/qperf/<case>/perf/<arch>/latest/` |
| freq | `99` |
| max-depth | `128` |
| mode | `tb` |
| format | `all` |
| top | `80` |
| min-percent | `0.3` |
| host-time | enabled unless `--no-host-time` |

### `cargo starry run --perf`

`cargo starry run` 是 `cargo starry qemu` 的 alias。带 `--perf` 时转换为 `ArgsPerf` 并调用同一套 `perf::run()`：

```bash
cargo starry run \
  --perf \
  --perf-case blk-read \
  --perf-workload 'echo QPERF_BEGIN:blk; dd if=/usr/bin/lto-dump of=/dev/null bs=64k; echo QPERF_END:blk' \
  --perf-start-marker QPERF_BEGIN \
  --perf-stop-marker QPERF_END \
  --perf-qperf-metrics
```

当前 `run --perf` 支持默认 qperf QEMU/rootfs flow，不支持和 `--qemu-config`、`--rootfs`、`--config`、`--target`、`--smp` 混用；这些组合会给出明确错误，避免静默忽略。

## 4. 火焰图增强

新增能力：

* analyzer 支持 `--symbol-style full|short|module`。
* analyzer fallback symtab 也会做 Rust demangle。
* analyzer 支持 `--focus <regex>` 生成聚焦 folded/flamegraph。
* analyzer 支持 `--min-percent` 控制 SVG frame 最小宽度。
* `cargo starry perf` 输出默认、workload、boot、post、focus 多个火焰图路径。
* `--full-stack` 会把 qperf plugin max depth 至少提升到 256。
* `--no-truncate` 会把 flamegraph min width 降到 0。

真实验证中，新 analyzer 已能把旧 `_R...` mangled symbol 转换为可读 Rust 路径，例如：

```text
<virtio_drivers::queue::VirtQueue<ax_driver::virtio::VirtIoHalImpl, 16usize>>::add_notify_wait_pop
<ax_alloc::buddy_slab::GlobalAllocator>::alloc
```

但当前 qperf 仍不保证恢复截图级完整调用栈。根因是 stack unwind 仍依赖 frame pointer 链，QEMU plugin sample 只记录 IP trace，没有 DWARF unwind 状态、vCPU/thread 元数据或 guest backtrace。

## 5. 使用示例

boot：

```bash
cargo starry perf --case boot
```

blk-read：

```bash
cargo starry perf \
  --case blk-read \
  --qperf-metrics \
  --start-marker QPERF_BEGIN \
  --stop-marker QPERF_END \
  --workload-timeout 45 \
  --workload 'echo reset > /proc/qperf_metrics; echo QPERF_BEGIN:blk-read; dd if=/usr/bin/lto-dump of=/dev/null bs=64k; cat /proc/qperf_metrics; echo QPERF_END:blk-read'
```

net-wget：

```bash
cargo starry perf \
  --case net-wget \
  --qperf-metrics \
  --start-marker QPERF_BEGIN \
  --stop-marker QPERF_END \
  --workload 'echo reset > /proc/qperf_metrics; echo QPERF_BEGIN:net-wget; wget -O /dev/null http://10.0.2.2:8000/tmp/axbuild/rootfs/rootfs-riscv64-alpine.img.tar.xz; cat /proc/qperf_metrics; echo QPERF_END:net-wget'
```

在 Docker/WSL 下，net server 应与 QEMU 处在可达网络拓扑中；此前验证显示 WSL host 上直接启动 HTTP server 时 guest 访问 `10.0.2.2:8000` 会 connection refused。

A/B compare：

```bash
python3 tools/starry-syscall-harness/harness.py perf-compare \
  --baseline target/qperf/blk-read/perf/riscv64/latest/report.json \
  --candidate target/qperf/blk-read-patched/perf/riscv64/latest/report.json \
  --name blk-read-ab \
  --output-dir target/qperf/compare
```

## 6. 验收结果

| 验收项 | 结果 | 证据路径 | 备注 |
| --- | --- | --- | --- |
| qperf-analyzer help | PASS | `target/qperf-integration-smoke/help/qperf-analyzer-help.txt` | `--symbol-style`、`--focus`、`--min-percent` 出现在 help |
| harness perf-profile help | PASS | `target/qperf-integration-smoke/help/harness-perf-profile-help.txt` | 旧 harness 入口保留，并暴露新增 flamegraph 参数 |
| harness perf-postprocess help | PASS | `target/qperf-integration-smoke/help/harness-perf-postprocess-help.txt` | cargo 入口复用此 postprocess 生成 report/csv |
| cargo starry perf help | PASS | `target/qperf-integration-smoke/help/cargo-starry-perf-help.txt` | 使用临时 `CARGO_HOME` 与测试用 `PKG_CONFIG_PATH` 通过；当前 host 原生环境仍缺 `libudev.pc` |
| cargo starry run help | PASS | `target/qperf-integration-smoke/help/cargo-starry-run-help.txt` | `run` alias 暴露 `--perf`、`--perf-case`、`--perf-workload`、`--perf-focus` 等参数 |
| axbuild check/clippy | PASS | command output | `cargo check -p axbuild`、`cargo clippy -p axbuild -- -D warnings` 通过 |
| cargo CLI parse tests | PASS | command output | `command_parses_perf_*` 与 `command_parses_run_perf_alias` 通过 |
| qperf plugin check | PASS | command output | `cargo check --manifest-path tools/qperf/Cargo.toml` 通过 |
| analyzer flamegraph resolve | PASS | `target/qperf-integration-smoke/analyzer/flamegraph.svg` | 复用已有 blk raw sample 生成 SVG |
| analyzer focused flamegraph | PASS | `target/qperf-integration-smoke/analyzer/flamegraph.virtio.svg` | `--focus 'virtio|VirtQueue'` 输出 159 samples |
| stack.folded symbol 粒度 | PARTIAL | `target/qperf-integration-smoke/analyzer/stack.full.folded` | symbol demangle 可读；调用链仍常短栈 |
| harness perf-postprocess | PASS | `target/qperf-integration-smoke/postprocess/report.json` | 从既有 qperf artifacts 生成 report/csv，参数中包含 `symbol_style/focus/no_truncate` |
| cargo starry perf boot | FAIL | `target/qperf-integration-smoke/logs/cargo-starry-perf-boot.log` | qperf tools、StarryOS build、rootfs 准备均已推进；宿主缺 `qemu-system-riscv64`，未产生 raw samples/report |
| cargo starry perf blk | NOT RUN | N/A | 与 boot 相同的 QEMU host 依赖阻塞，未重复下载/运行 |
| old full harness QEMU run | NOT RUN | N/A | 本轮未再触发 Docker profile，避免重新生成 root-owned `tmp/axbuild`；CLI/help 兼容已验证 |

本轮真实 boot profile 阻塞原因：

```text
Error: qperf requires `qemu-system-riscv64` in PATH; install the matching QEMU system emulator or run the Docker-based harness perf-profile entrypoint
```

额外环境说明：

* 当前 host 原生 `pkg-config` 找不到 `libudev.pc`。help/check 验证使用 `/tmp/fake-pkgconfig` 作为只用于编译帮助/测试的替代，不代表生产运行环境；正常环境应安装 `libudev-dev`。
* 默认 Cargo registry cache 仍有 root-owned cache warning；验证使用 `CARGO_HOME=/tmp/tgoskits-cargo-home` 避免写入用户 cache。
* 原仓库 `tmp/axbuild/` 存在 root-owned 文件；本轮已通过 `AXBUILD_TMP_DIR` 和 cargo perf 默认 `axbuild-tmp/` 隔离规避。
* `rust-objcopy` 初始不在 PATH；验证时临时加入 Rust toolchain 的 llvm-tools 路径，随后现有 `starry-kallsyms.sh` 安装了 `cargo-binutils` 与 `gen_ksym` 到 `/tmp/tgoskits-cargo-home/bin`。

## 7. 局限性

* `cargo starry perf` 代码路径已实现并推进到 QEMU 启动前，但当前 host 缺 `qemu-system-riscv64`，所以 boot/blk profile 未能生成新的 raw samples 和最终 report。
* qperf 仍是 QEMU TCG plugin 采样，不是 guest PMU。
* marker window 仍是 timestamp 后处理过滤，不是 runtime pause/resume。
* stack unwind 仍依赖 frame pointer，不能保证完整 Rust 调用链。
* counters 仍是 driver-visible 近似值，不是 ring-level 精确统计。
* host perf/PMU 是可选 host QEMU process 指标。
* vsock 仍受 `/dev/vhost-vsock` 限制。

## 8. 下一步建议

1. 在安装 `qemu-system-riscv64`、`libudev-dev`，并保证 `rust-objcopy` 在 PATH 的 host 上重跑 `cargo starry perf --case boot`。
2. 重跑 blk marker profile，确认 cargo 入口完整生成 `report.json/report.md/hotspots.csv/hotspot_categories.csv/qperf/flamegraph.svg`。
3. cargo 入口闭环后开始 net RX 去 `copy_within()` A/B。
4. 开始 net inflight `BTreeMap` 到 fixed array/slab 的 A/B。
5. 开始 blk pending read / async queue 原型 A/B。
6. 继续探索 ring-level virtqueue counters 和 runtime pause/resume sampling。
