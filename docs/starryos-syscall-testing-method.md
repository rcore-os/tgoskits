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
- **`ensure-starry-base-rootfs.sh`**：若缺 **`target/.../rootfs-riscv64.img`** 则自动 **`cargo xtask starry rootfs --arch riscv64`**（`prepare-rootfs-with-probe.sh` 与 **`run-smp2-guest-matrix.sh`** 共用）。
- **`run-smp2-guest-matrix.sh`**：全 contract 在 **`-smp 2`** 下跑 guest，并对 **`expected/*.line`** 做 **`verify-guest-log-oracle.sh`**（见 `docs/starryos-syscall-smp-notes.md`）。
- **`run-starry-probe-qemu.sh <probe>`**：依次执行注入镜像与 `cargo xtask starry test qemu`（见 `test-suit/starryos/probes/README.md`）。
- **`verify-guest-log-oracle.sh <probe> [log|-]`**：从串口/日志取首行 `^CASE `，与 `expected/<probe>.line` 自动比对（**0 / 1 / 2** 退出码）。**可不写第二个参数**，从标准输入读入（粘贴串口全文后 **Ctrl+D**）；或用完整 **`cargo xtask starry test qemu … 2>&1 | tee serial.log`** 留档后再验（完整命令见 **`test-suit/starryos/probes/README.md`**「方式 B」）。
- **`extract-case-line.sh`** / **`diff-guest-line.sh`**：底层抽取与单行比对。
- **`scripts/starryos-probes-ci.sh`**：catalog 校验、覆盖检查、shell `sh -n`、可选交叉编译（无需 QEMU）；若仅有 **`riscv64-linux-gnu-gcc`**（如 Ubuntu）也会尝试构建。
- **`test-suit/starryos/scripts/run-e2e-probe-smoke.sh`**：本地 **rootfs + 注入 + `cargo xtask starry test qemu`** 一键冒烟（默认不跑 CI）。

**GitHub Actions**：
- `.github/workflows/starryos-probes.yml` — 静态 job + `linux-oracle`（push / `workflow_dispatch`）。
- `.github/workflows/starryos-probes-smp2-matrix.yml` — **SMP2 全 contract guest 矩阵**（仅 **`workflow_dispatch`** 与 **每日 UTC 02:00** `schedule`，不在 push 上跑）。

**日常命令速查**：`docs/starryos-probes-daily.md`。

**提交分组**：见 `docs/starryos-syscall-commit-strategy.md`。

**SMP**：见 `docs/starryos-syscall-smp-notes.md`（`-smp 2` TOML、单探针脚本与全量矩阵已落地）。

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

## 生成器与手写 contract 的分工

- **`scripts/gen_syscall_probes.py`**：按 catalog **`generator_hints.template`** 写 **`probes/generated/<syscall>_generated.c`**。脚本顶部 docstring 与下表一致。
- **已接模板的稳定形状**：`contract_write_zero`、`contract_read_zero`、`contract_execve_enoent`、`contract_wait4_echild` — 生成结果应与 **`probes/contract/*.c`** 中同名语义探针一致；**oracle 与 CI 以 `contract/` + `expected/*.line` 为准**。
- **`contract_errno` / `contract_stub` / 未知模板**：仅 **`emit_stub`** 占位，**不能**直接当作 oracle；需新增手写 `contract/*.c` 后再把 catalog **`tests:`** 指过去。
- **`futex` / `ppoll`**：保持 stub；**不要**加固定 **`expected/*.line`** 或把其纳入「单行 CASE」矩阵，除非已有 **SMP 安全、输出确定** 的设计（见兼容矩阵 **`parity: not_applicable`**）。

## StarryOS 与 Linux 不一致时的处理

1. **先确认**：同一探针在 **`qemu-riscv64`（user oracle）** 与 **StarryOS guest 串口** 的差异是否可复现。
2. **修内核**：若属 bug，以 Linux 行为为锚点修复 StarryOS。
3. **保留差异并文档化**：在 **`docs/starryos-syscall-compat-matrix.yaml`** 将 **`parity`** 标为 **`divergent`**，**`notes`** 写清原因与 issue。
4. **拆分期望行（少用）**：仅当必须保留双轨语义时，增加 guest 专用期望文件（例如 **`expected/<probe>.guest.line`**）并改 **`verify-guest-log-oracle.sh`** 或单独脚本 — **默认仍以单轨 `expected/<probe>.line`（Linux oracle）为主**。
