# StarryOS Probes — CI 示例（可选）

本仓库的 **`scripts/starryos-probes-ci.sh`** 可在任意 Linux runner 上运行（Python3 + PyYAML；可选 `riscv64-linux-musl-gcc`）。

若需在 CI 中强制执行 **Linux user-mode oracle**，可另开 job 安装 `qemu-user`（提供 `qemu-riscv64`），例如：

```yaml
# .github/workflows/starryos-probes.yml（示例片段，按需粘贴调整）
jobs:
  starryos-probes-static:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Dependencies
        run: |
          sudo apt-get update
          sudo apt-get install -y python3-yaml
      - name: Static checks + optional build
        run: ./scripts/starryos-probes-ci.sh

  starryos-probes-oracle:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Dependencies
        run: |
          sudo apt-get update
          sudo apt-get install -y python3-yaml qemu-user gcc-riscv64-linux-gnu
          # 若使用 musl 交叉链，改为安装对应包或缓存 toolchain
      - name: Build probes
        run: |
          export CC=riscv64-linux-musl-gcc
          test-suit/starryos/scripts/build-probes.sh
      - name: Oracle (strict)
        env:
          VERIFY_STRICT: "1"
        run: test-suit/starryos/scripts/run-diff-probes.sh verify-oracle-all
```

说明：第二条 job 的编译器需与本地约定一致（`riscv64-linux-musl-gcc` 或发行版提供的 `riscv64-linux-gnu-gcc`）；若工具链不同，应同步更新 `build-probes.sh` 的默认 `CC` 与 `expected/*.line` 的生成环境说明。
