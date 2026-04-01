# StarryOS Linux Syscall 测试方法（渐进式）

本文描述仓库内 **Linux oracle 探针** 与 **StarryOS QEMU 回归** 的分工，便于扩展更多 syscall contract。

## 分层

1. **分发表真相源**：`scripts/extract_starry_syscalls.py` 从 `handle_syscall` 的 `match` 生成 `docs/starryos-syscall-dispatch.json`。
2. **Catalog**：`docs/starryos-syscall-catalog.yaml` 记录优先级、风险标签、实现路径与关联探针路径；与分发表一致性用 `--check-catalog` 校验。
3. **探针**
   - **手写 contract**：`test-suit/starryos/probes/contract/*.c`，命名建议 `<syscall>_<scenario>.c`，产出静态 riscv64 ELF。
   - **生成骨架**：`scripts/gen_syscall_probes.py` 按 `generator_hints.template` 写入 `probes/generated/`（占位，逐步替换为手写或半自动生成）。
4. **Oracle 行**：`test-suit/starryos/probes/expected/<probe_basename>.line`，单行、可 `diff`；与 `qemu-riscv64` 下 stdout 对齐。
5. **Guest 回归**：`prepare-rootfs-with-probe.sh <basename>` 注入 `/root/<basename>`；`cargo xtask starry test qemu --test-disk-image … --shell-init-cmd test-suit/starryos/testcases/probe-<basename>-0`。

## 辅助脚本

- **`scripts/check_probe_coverage.py`**：校验 catalog 中 `tests:` 所列路径均在仓库中存在。
- **`run-diff-probes.sh`**：设置 **`VERIFY_STRICT=1`** 时，若缺少 `qemu-riscv64`，`verify-oracle` / `verify-oracle-all` 以退出码 **2** 失败（便于 CI 要求必须跑 oracle）。
- **`diff-guest-line.sh`**：将串口/日志中的一行 `CASE …` 与 `expected/<probe>.line` 比对。
- **`run-starry-probe-qemu.sh <probe>`**：依次执行注入镜像与 `cargo xtask starry test qemu`（见 `test-suit/starryos/probes/README.md`）。
- **`extract-case-line.sh`**：从日志或管道中取首行 `CASE …`（便于与 `diff-guest-line.sh` 串联）。
- **`scripts/starryos-probes-ci.sh`**：catalog 校验、覆盖检查、shell `sh -n`、可选交叉编译（无需 QEMU）。

**提交分组**：见 `docs/starryos-syscall-commit-strategy.md`。

**SMP**：见 `docs/starryos-syscall-smp-notes.md`（占位，对应路线图 S0-6）。

## 新增一条 syscall contract 的检查清单

- [ ] 在 catalog 增加条目并 `extract_starry_syscalls.py --check-catalog`。
- [ ] 添加 `contract/*.c` 与 `expected/*.line`。
- [ ] `python3 scripts/check_probe_coverage.py` 通过。
- [ ] `./scripts/starryos-probes-ci.sh` 通过（合并前按 `docs/starryos-syscall-commit-strategy.md` 分组提交更佳）。
- [ ] `build-probes.sh` 已自动编译全部 `contract/*.c`。
- [ ] `run-diff-probes.sh verify-oracle-all`（需 `qemu-riscv64`）。
- [ ] 增加 `testcases/probe-<name>-0` 与 `prepare-rootfs-with-probe.sh <name>` 试跑文档中的 QEMU 命令。

## 与 Linux 行为对齐

Contract 应优先选取 **跨 libc 稳定** 的边界（如 `EBADF` 的 errno 数值、零长度 `write` 返回值）。若平台差异大，应在 `expected` 文件名或 catalog `notes` 中标明仅针对 `riscv64` + `musl` oracle。
