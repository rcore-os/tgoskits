# StarryOS Probes — CI 说明

## 已接入的工作流

仓库根目录 **`.github/workflows/starryos-probes.yml`** 已实现：

1. **static**：安装 `python3-yaml` 后运行 **`./scripts/starryos-probes-ci.sh`**（catalog、覆盖、shell 语法；若 runner 上存在 `riscv64-linux-gnu-gcc` 或 `riscv64-linux-musl-gcc` 则顺带交叉编译）。
2. **linux-oracle**：安装 `python3-yaml`、`qemu-user`、`gcc-riscv64-linux-gnu`，用 **GNU** 静态链接构建探针后执行 **`VERIFY_STRICT=1`** 的 **`verify-oracle-all`**。

3. **SMP2 guest 矩阵（重）**：**`.github/workflows/starryos-probes-smp2-matrix.yml`** — 仅在 **`workflow_dispatch`** 与 **定时（UTC 02:00）** 运行；在 **`ghcr.io/<repo>-container`** 内执行 **`run-smp2-guest-matrix.sh`**（`CC=riscv64-linux-gnu-gcc`），上传 **`starry-smp2-matrix-logs/`** artifact。不在 push 上触发，避免拖慢默认 CI。

推送命中 `test-suit/starryos/**`、相关 `scripts/`、`docs/starryos-syscall-catalog.yaml` 等路径时会自动跑 **static + linux-oracle**；也可在 **Actions → StarryOS syscall probes → Run workflow** 手动触发。SMP2 矩阵在 **StarryOS probes SMP2 guest matrix** 工作流中单独手动或等 nightly。

## 与本地 musl 的差异

本地开发推荐 **`riscv64-linux-musl-gcc`**；CI oracle job 使用 **`riscv64-linux-gnu-gcc`** 以便 `apt` 一键安装。当前 contract 以 **errno 数值与返回码** 为主，两套工具链在 Ubuntu 上应对齐；若出现 `expected/*.line` 不一致，再为 CI 单独维护期望文件或改为容器内 musl 构建。

---

以下为历史「粘贴示例」片段，可与现网 workflow 对照：

```yaml
jobs:
  starryos-probes-static:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: sudo apt-get update && sudo apt-get install -y python3-yaml
      - run: ./scripts/starryos-probes-ci.sh

  starryos-probes-oracle:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: sudo apt-get update && sudo apt-get install -y python3-yaml qemu-user gcc-riscv64-linux-gnu
      - run: CC=riscv64-linux-gnu-gcc test-suit/starryos/scripts/build-probes.sh
      - env: { VERIFY_STRICT: "1" }
        run: test-suit/starryos/scripts/run-diff-probes.sh verify-oracle-all
```
