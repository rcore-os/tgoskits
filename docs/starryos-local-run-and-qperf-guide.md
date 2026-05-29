# 本机启动 StarryOS 与 qperf 的步骤指南

本文说明如何在本机工作区一步步启动 StarryOS、运行 qperf 性能采样、查看报告和火焰图。这里的“本机”指你在宿主机上的 tgoskits 仓库目录发起操作；所有 StarryOS 构建、rootfs、QEMU 和 qperf profile 都应放在 Docker 容器中执行。

默认镜像：

```text
ghcr.io/rcore-os/tgoskits-container:latest
```

推荐工作目录：

```bash
cd /home/cg24/tgoskits
```

## 1. 基本原则

不要在宿主机直接跑 StarryOS/QEMU/qperf。原因有三点：

1. 工具链、交叉编译器、QEMU 版本和 rootfs 工具都已经在项目容器里配好。
2. 直接在宿主机跑会污染本地 cargo cache、target 和 rootfs 产物，权限也容易变乱。
3. harness 和 PR 验证都默认 StarryOS 相关流程在 Docker 内完成。

宿主机可以做这些事：

- 调用 `harness.py`，让它自动 re-enter Docker。
- 启动本地 Web UI。
- 打开报告、Markdown、CSV、SVG。
- 读写源码和文档。

容器内做这些事：

- `cargo xtask starry defconfig`
- `cargo xtask starry rootfs`
- `cargo xtask starry qemu`
- `cargo xtask starry perf`
- StarryOS 相关测试和 clippy。

## 2. 一次性检查环境

先确认 Docker 可用，并且镜像能拉到：

```bash
docker pull ghcr.io/rcore-os/tgoskits-container:latest
```

再从宿主机跑 harness doctor：

```bash
python3 tools/starry-syscall-harness/harness.py doctor
```

`doctor` 会检查 Docker、镜像、容器内 Python/Cargo/QEMU 等基础工具。这个命令本身可以在宿主机运行；harness 会负责进入 Docker。

## 3. 推荐方式：用 harness 跑 qperf

如果目标是拿 StarryOS 性能报告和 flamegraph，优先用 harness：

```bash
python3 tools/starry-syscall-harness/harness.py perf-profile \
  --arch riscv64 \
  --timeout 20 \
  --format all \
  --freq 99 \
  --max-depth 64 \
  --mode tb \
  --top 20 \
  --min-percent 5.0
```

这条命令会自动做这些事：

1. 在 Docker 中执行 StarryOS/qperf 流程。
2. 刷新 `qemu-riscv64` defconfig。
3. 构建 qperf plugin 和 qperf analyzer。
4. 构建 StarryOS release kernel。
5. 准备 rootfs。
6. 启动 QEMU 并注入 qperf plugin。
7. 收集 `qperf.bin` raw samples。
8. 解析 folded stack。
9. 生成 flamegraph。
10. 写出 JSON/Markdown/CSV 报告。

输出目录固定在：

```text
target/starry-syscall-harness/perf/riscv64/latest/
```

重点文件：

```text
target/starry-syscall-harness/perf/riscv64/latest/report.json
target/starry-syscall-harness/perf/riscv64/latest/report.md
target/starry-syscall-harness/perf/riscv64/latest/hotspots.csv
target/starry-syscall-harness/perf/riscv64/latest/qperf/stack.folded
target/starry-syscall-harness/perf/riscv64/latest/qperf/flamegraph.svg
target/starry-syscall-harness/perf/riscv64/latest/profile.stderr
```

如果命令结束时看到类似：

```text
result: ok
samples: <non-zero>
```

说明至少采到了有效样本。

## 4. 启动本地 Web UI 查看报告

如果想在浏览器里点按钮、看报告和火焰图，可以启动本地 UI：

```bash
python3 tools/starry-syscall-harness/harness.py ui \
  --host 127.0.0.1 \
  --port 8765 \
  --open
```

浏览器地址：

```text
http://127.0.0.1:8765
```

UI 支持：

- Doctor 检查；
- syscall scan；
- qperf profile；
- perf diff；
- 查看 `report.json` / `report.md`；
- 查看 `flamegraph.svg`。

注意：UI 只是交互入口。真正的 StarryOS/QEMU/qperf 仍然由 harness 放进 Docker 执行。

## 5. 手动进入 Docker shell

如果你想一步步看 StarryOS 怎么起，建议开一个交互式 Docker shell：

```bash
cd /home/cg24/tgoskits

docker run --rm -it \
  -v "$PWD":/work \
  -w /work \
  -e HOST_UID="$(id -u)" \
  -e HOST_GID="$(id -g)" \
  ghcr.io/rcore-os/tgoskits-container:latest \
  bash
```

进入容器后，工作目录是：

```bash
/work
```

后续本节命令都在容器内执行。

退出容器前建议修正产物属主，避免宿主机看到 root-owned 文件：

```bash
chown -R "$HOST_UID:$HOST_GID" target tmp tools/qperf/target 2>/dev/null || true
exit
```

如果你用 harness 自动跑，它会自己处理常见产物目录的属主问题。

## 6. 手动准备 StarryOS QEMU 配置

在容器内先生成 QEMU defconfig：

```bash
cargo xtask starry defconfig qemu-riscv64
```

这个命令会生成或刷新 StarryOS 的临时构建配置。建议每次 rebase、切换分支、平台配置变动后都先跑一次，避免复用旧的 `tmp/axbuild` 配置。

可查看支持的 StarryOS 平台：

```bash
cargo xtask starry config ls
```

也可以看 quick-start 支持项：

```bash
cargo xtask starry quick-start list
```

## 7. 手动准备 rootfs

在容器内准备 riscv64 rootfs：

```bash
cargo xtask starry rootfs --arch riscv64
```

成功后会看到类似：

```text
rootfs ready at /work/tmp/axbuild/rootfs/rootfs-riscv64-alpine.img
```

这个 rootfs 是 QEMU 启动 StarryOS 用户态环境的磁盘镜像。`starry qemu` 和 `starry perf` 通常会自动确保 rootfs 存在，但手动拆步骤时先跑一遍更清楚。

## 8. 手动构建 StarryOS

release 构建：

```bash
cargo xtask starry build --arch riscv64
```

debug 构建：

```bash
cargo xtask starry build --arch riscv64 --debug
```

性能分析默认使用 release。debug 更适合查符号和栈形态，但不适合做最终性能结论。

构建产物通常在：

```text
target/riscv64gc-unknown-none-elf/release/starryos
target/riscv64gc-unknown-none-elf/release/starryos.bin
```

## 9. 手动启动 StarryOS QEMU

最直接的启动命令：

```bash
cargo xtask starry qemu --arch riscv64
```

这个命令会：

1. 读取 StarryOS build config。
2. 确保 rootfs 可用。
3. patch QEMU args，把 rootfs 挂到虚拟磁盘。
4. 构建 kernel。
5. 启动 QEMU。

成功进入 guest 后，通常能看到 StarryOS shell，例如：

```text
root@starry:/root #
```

可以在 guest 里试：

```sh
pwd
ls /
cat /proc/cpuinfo
```

退出方式：

```sh
poweroff
```

如果 guest 没正常关机，可以用 QEMU 的 nographic 退出键：

```text
Ctrl-A X
```

也就是先按 `Ctrl-A`，再按 `x`。

## 10. quick-start 启动 StarryOS

也可以用 quick-start，适合只想快速起机：

```bash
cargo xtask starry quick-start qemu-riscv64 build
cargo xtask starry quick-start qemu-riscv64 run
```

quick-start 会选择对应平台模板，并把常见步骤串起来。手动排查 qperf 时，我更推荐显式跑：

```bash
cargo xtask starry defconfig qemu-riscv64
cargo xtask starry rootfs --arch riscv64
cargo xtask starry qemu --arch riscv64
```

这样每一步失败点更清楚。

## 11. 手动运行 qperf

如果不用 harness，也可以在容器内直接调用 `cargo xtask starry perf`：

```bash
cargo xtask starry perf \
  --arch riscv64 \
  --timeout 20 \
  --format all \
  --freq 99 \
  --max-depth 64 \
  --mode tb \
  --top 20 \
  --out target/qperf/manual-riscv64
```

这个命令会自动：

- 构建 qperf plugin：`tools/qperf/target/release/libqperf.so`；
- 构建 analyzer：`tools/qperf/target/release/qperf-analyzer`；
- 如果 `--format all` 或 `--format svg`，启用 analyzer 的 `flamegraph` feature；
- 构建 StarryOS；
- 准备 rootfs；
- 生成 qperf 专用 QEMU config；
- 启动 QEMU，并通过 `-plugin` 注入 qperf；
- 用 analyzer 解析 raw samples。

手动 qperf 输出目录：

```text
target/qperf/manual-riscv64/
```

关键文件：

```text
target/qperf/manual-riscv64/qperf.bin
target/qperf/manual-riscv64/qperf.summary.txt
target/qperf/manual-riscv64/summary.txt
target/qperf/manual-riscv64/stack.folded
target/qperf/manual-riscv64/flamegraph.svg
target/qperf/manual-riscv64/qemu.toml
```

如果只想要 folded stack，不要 SVG：

```bash
cargo xtask starry perf \
  --arch riscv64 \
  --timeout 20 \
  --format folded \
  --freq 99 \
  --max-depth 64 \
  --mode tb \
  --top 20 \
  --out target/qperf/manual-riscv64-folded
```

如果要更细粒度但更慢的 instruction callback：

```bash
cargo xtask starry perf \
  --arch riscv64 \
  --timeout 10 \
  --format all \
  --freq 101 \
  --max-depth 64 \
  --mode insn \
  --top 20 \
  --out target/qperf/manual-riscv64-insn
```

## 12. 打开 flamegraph

如果是在宿主机上看，可以直接打开：

```text
target/starry-syscall-harness/perf/riscv64/latest/qperf/flamegraph.svg
```

或者手动 qperf 的：

```text
target/qperf/manual-riscv64/flamegraph.svg
```

推荐用浏览器打开。当前 flamegraph 生成参数已经调宽，适合横向滚动查看。

如果想通过 UI 看：

```bash
python3 tools/starry-syscall-harness/harness.py ui \
  --host 127.0.0.1 \
  --port 8765 \
  --open
```

然后进入 Performance 页面查看 latest profile。

## 13. 比较两次 qperf 结果

先保存 baseline：

```bash
cp -a target/starry-syscall-harness/perf/riscv64/latest \
  target/starry-syscall-harness/perf/riscv64/baseline
```

修改代码后重新跑：

```bash
python3 tools/starry-syscall-harness/harness.py perf-profile \
  --arch riscv64 \
  --timeout 20 \
  --format all \
  --freq 99 \
  --max-depth 64 \
  --mode tb \
  --top 20
```

比较：

```bash
python3 tools/starry-syscall-harness/harness.py perf-diff \
  --baseline target/starry-syscall-harness/perf/riscv64/baseline \
  --compare target/starry-syscall-harness/perf/riscv64/latest \
  --top 20
```

`perf-diff` 比的是 folded stack 的样本占比变化。它能告诉你热点是否真的下降，但不能代替代码审查。看到下降后还要检查：

- 样本总数是否足够；
- `dropped_samples` 是否异常；
- workload 是否一致；
- release/debug 配置是否一致；
- 是否只是采样噪声。

## 14. 常见问题

### 没有 flamegraph.svg

先看：

```bash
cat target/starry-syscall-harness/perf/riscv64/latest/qperf/summary.txt
```

重点检查：

```text
flamegraph_generated = true
```

如果是 false 或文件不存在，常见原因是 analyzer 没启用 `flamegraph` feature。通过 harness 或 `cargo xtask starry perf --format all` 跑会自动处理。

### samples 是 0

看：

```bash
cat target/starry-syscall-harness/perf/riscv64/latest/profile.stderr
cat target/starry-syscall-harness/perf/riscv64/latest/qperf/summary.txt
```

常见原因：

- QEMU 在 plugin 写样本前就退出；
- timeout 太短；
- kernel filter 过滤过严；
- 地址 alias 没映射上；
- StarryOS 没成功进入预期 workload。

可以先把 timeout 拉长：

```bash
python3 tools/starry-syscall-harness/harness.py perf-profile \
  --arch riscv64 \
  --timeout 60 \
  --format all
```

### top function 是裸地址

例如：

```text
0x800065ae
_start+0x1000
```

通常是符号解析或地址 canonicalization 问题。检查 `profile.stderr` 中是否有：

```text
qperf: detected kernel .text virtual range
qperf: detected kernel .text physical alias
```

如果没有，说明 kernel ELF 或 axconfig 没被正确找到。

### QEMU 配置突然坏了

切分支或 rebase 后先刷新 defconfig：

```bash
docker run --rm -it \
  -v "$PWD":/work \
  -w /work \
  ghcr.io/rcore-os/tgoskits-container:latest \
  bash -lc 'cargo xtask starry defconfig qemu-riscv64'
```

如果还不行，可以清理生成配置后重试：

```bash
rm -rf tmp/axbuild/config/starryos
cargo xtask starry defconfig qemu-riscv64
```

这条清理命令只应在你确认要丢弃本地生成配置时执行。

### 宿主机文件变成 root-owned

如果手动进 Docker 跑过命令，退出前执行：

```bash
chown -R "$HOST_UID:$HOST_GID" target tmp tools/qperf/target 2>/dev/null || true
```

如果已经退出容器，可以在宿主机用 sudo 修：

```bash
sudo chown -R "$(id -u):$(id -g)" target tmp tools/qperf/target
```

## 15. 推荐日常流程

只想启动 StarryOS：

```bash
docker run --rm -it \
  -v "$PWD":/work \
  -w /work \
  -e HOST_UID="$(id -u)" \
  -e HOST_GID="$(id -g)" \
  ghcr.io/rcore-os/tgoskits-container:latest \
  bash

cargo xtask starry defconfig qemu-riscv64
cargo xtask starry rootfs --arch riscv64
cargo xtask starry qemu --arch riscv64
chown -R "$HOST_UID:$HOST_GID" target tmp tools/qperf/target 2>/dev/null || true
exit
```

只想拿 qperf 报告：

```bash
python3 tools/starry-syscall-harness/harness.py perf-profile \
  --arch riscv64 \
  --timeout 20 \
  --format all \
  --freq 99 \
  --max-depth 64 \
  --mode tb \
  --top 20
```

想交互式看报告：

```bash
python3 tools/starry-syscall-harness/harness.py ui \
  --host 127.0.0.1 \
  --port 8765 \
  --open
```

修改 StarryOS 逻辑后验证：

```bash
docker run --rm \
  -v "$PWD":/work \
  -w /work \
  ghcr.io/rcore-os/tgoskits-container:latest \
  bash -lc 'cargo fmt --all --check && cargo xtask clippy --package starry-kernel'
```

如果改动影响 syscall 行为，再跑：

```bash
python3 tools/starry-syscall-harness/harness.py discover \
  --arch riscv64 \
  --timeout 120 \
  --fail-on-diff
```

如果改动影响性能，再跑 qperf profile 和 perf diff。

## 16. 相关文档

更偏实现和问题分析的 qperf 采样记录见：

```text
docs/qperf-sampling-debug-notes.md
```

harness 总览见：

```text
docs/starry-syscall-harness.md
tools/starry-syscall-harness/README.md
```
