# StarryOS Linux Syscall 渐进式工程 — 迭代纪要

本文档按轮次记录每轮目标、交付物与验证要点。

---

## 第 1 轮 — Catalog 扩展与生成链

**目标**：在可追溯前提下纳入第二个高价值 syscall（`close`），并保持分发表 / 生成器一致。

**交付物**：`docs/starryos-syscall-catalog.yaml` 新增 `close` 条目（`contract_errno` 模板、`tests` 指向手写 contract）；`python3 scripts/extract_starry_syscalls.py --check-catalog` 通过；`gen_syscall_probes.py` 写出 `generated/close_generated.c`（10 个骨架）。

**验证**：`Catalog check OK (10 entries)`；`Wrote 10 skeleton(s)`。

---

## 第 2 轮 — `close` 手写 contract 与 oracle 行

**目标**：提供可重复的 Linux 侧语义锚点（非法 fd → `EBADF`）。

**交付物**：`test-suit/starryos/probes/contract/close_badfd.c`；`expected/close_badfd.line`（`errno=9`）。

**验证**：本环境已用 `riscv64-linux-musl-gcc` 成功静态链接；`qemu-riscv64` 未装时 oracle 留待本机补跑。

---

## 第 3 轮 — 多探针 oracle 脚本

**目标**：`verify-oracle` 从「写死 write_stdout」改为按探针名选期望文件，并支持一键全量。

**交付物**：重写 `test-suit/starryos/scripts/run-diff-probes.sh`：`verify-oracle [name]` 使用 `expected/<name>.line`；新增 `verify-oracle-all`。

**验证**：脚本 `sh -n` 通过；逻辑上对每个 `*.line` 调用对应 `build-riscv64/<basename>`。

---

## 第 4 轮 — 测试方法文档（S0-1 子集）

**目标**：把分层模型与扩展 checklist 固定成文，降低后续贡献者心智负担。

**交付物**：`docs/starryos-syscall-testing-method.md`。

**验证**：与现有目录布局、`xtask` 流程一致。

---

## 第 5 轮 — 兼容矩阵骨架（S0-4 子集）

**目标**：从「只有 catalog」前进到「可对齐结论」的占位结构。

**交付物**：`docs/starryos-syscall-compat-matrix.yaml`（`write` / `close` / `openat` 示例行）。

**验证**：YAML 可人工编辑；后续可接校验脚本或生成表格。

---

## 第 6 轮 — 通用 rootfs 注入脚本

**目标**：避免每增加一个探针就复制一整份 shell。

**交付物**：`prepare-rootfs-with-probe.sh <basename>`；`write_stdout` 仍输出 **`rootfs-riscv64-probe.img`**（兼容既有文档）；其它探针为 `rootfs-riscv64-probe-<name>.img`。

**验证**：`prepare-rootfs-with-write_stdout-probe.sh` 改为 `exec` 包装器，行为与旧路径一致。

---

## 第 7 轮 — StarryOS QEMU 用例：`close_badfd`

**目标**：第二条可在 `starry test qemu` 路径上跑的 guest 脚本。

**交付物**：`test-suit/starryos/testcases/probe-close_badfd-0`。

**验证**：需本地 `prepare-rootfs-with-probe.sh close_badfd` + `--test-disk-image …probe-close_badfd.img`（命令见 `probes/README.md`）。

---

## 第 8 轮 — 探针 README 与索引脚本

**目标**：一眼看到已有 contract、如何批量 oracle、如何接新 syscall。

**交付物**：`test-suit/starryos/probes/README.md` 增补表格、`verify-oracle-all`、通用 QEMU 段落、文档链接；`list-contract-probes.sh`。

**验证**：`list-contract-probes.sh` 列出 `close_badfd`、`write_stdout`。

---

## 第 9 轮 — S0 批次总结同步

**目标**：让「单点总结」与仓库现状一致，避免同事仍以为只有 9 条 catalog / 单探针。

**交付物**：更新 `docs/starryos-syscall-s0-batch-summary.md`（条目数、脚本、文档链接、`verify-oracle-all`）。

**验证**：交付物表与上文路径一致。

---

## 第 10 轮 — 本轮总览与后续缺口

**目标**：收束 10 轮增量，并标明仍未做的路线图项。

**交付物**：本文件十段纪要；工作区已具备 **多探针 oracle 入口**、**通用镜像注入**、**方法与矩阵骨架文档**（手写 contract 数量见第 20 轮收束）。

**仍建议后续迭代**：安装 `qemu-riscv64` 后跑通 `verify-oracle-all`；guest 串口输出与 oracle **自动 diff**；**S0-6** SMP 用例矩阵；catalog 中其余 syscall 逐步替换 `generated` stub 为真实 contract；`git commit` 分组提交。

---

## 第 11 轮 — Catalog：`read` + 生成模板 `contract_read_zero`

**目标**：与 `write` 零长度对称，覆盖 **`read(2)` count=0** 的稳定边界。

**交付物**：`docs/starryos-syscall-catalog.yaml` 增加 `read`；`scripts/gen_syscall_probes.py` 新增 `emit_read_zero` / `contract_read_zero`；`gen_syscall_probes.py` 写出 `generated/read_generated.c`。

**验证**：`extract_starry_syscalls.py --check-catalog`；`gen_syscall_probes.py` 报告 11 个骨架。

---

## 第 12 轮 — 手写 `read_stdin_zero` 与 QEMU 用例

**目标**：第三条可在 guest 中执行的静态探针。

**交付物**：`contract/read_stdin_zero.c`、`expected/read_stdin_zero.line`、`testcases/probe-read_stdin_zero-0`。

**验证**：`build-probes.sh` 编译；`list-contract-probes.sh` 列出三名。

---

## 第 13 轮 — `check_probe_coverage.py`

**目标**：防止 catalog 写了 `tests:` 但仓库漏文件。

**交付物**：`scripts/check_probe_coverage.py`。

**验证**：对当前 catalog 输出 `Probe coverage OK`。

---

## 第 14 轮 — Oracle 严格模式（CI）

**目标**：默认 SKIP 友好，CI 可强制要求 user-mode QEMU。

**交付物**：`run-diff-probes.sh` 支持 **`VERIFY_STRICT=1`**；缺 `qemu-riscv64` 时 `verify_one` 返回 **2**；`verify-oracle-all` 遇严格失败 **exit 2**。

**验证**：`sh -n`；手测 `VERIFY_STRICT=1 verify-oracle-all` 在无 qemu 环境为退出码 2。

---

## 第 15 轮 — `diff-guest-line.sh`

**目标**：把「串口一行 vs `expected/*.line`」封装成可脚本化步骤。

**交付物**：`test-suit/starryos/scripts/diff-guest-line.sh`。

**验证**：传入与期望一致的 `CASE` 行时退出 0。

---

## 第 16 轮 — `run-starry-probe-qemu.sh`

**目标**：减少复制粘贴 `prepare` + `cargo xtask` 的长度。

**交付物**：`test-suit/starryos/scripts/run-starry-probe-qemu.sh`（处理 `write_stdout` 镜像名特例）。

**验证**：`sh -n`；需完整 rootfs/QEMU 环境时再做端到端。

---

## 第 17 轮 — SMP 占位文档（S0-6）

**目标**：明确当前探针默认单核、多核后续接法。

**交付物**：`docs/starryos-syscall-smp-notes.md`。

**验证**：与 catalog 中 `smp2` 标注方向一致。

---

## 第 18 轮 — 兼容矩阵与测试方法同步

**目标**：矩阵与 README 反映 `read` 探针；方法文档收录新脚本。

**交付物**：更新 `docs/starryos-syscall-compat-matrix.yaml`、`docs/starryos-syscall-testing-method.md`、`test-suit/starryos/probes/README.md`、`docs/starryos-syscall-s0-batch-summary.md`。

**验证**：文档内路径与仓库文件一致。

---

## 第 19 轮 — 文档交叉引用

**目标**：从 probes README 可发现 SMP 说明、覆盖检查、严格 oracle。

**交付物**：见第 18 轮 probes README 增补（本轮侧重可发现性）。

**验证**：新贡献者按 README 可跑通检查链前段（无需 QEMU）。

---

## 第 20 轮 — 第 11–20 轮收束

**目标**：固定本批次范围，便于审计。

**交付物**：本文件第 11–20 节；当时为 **3 个手写 contract**、**11 条 catalog**；后续第 21–30 轮已扩展（见第 30 轮收束）。

**仍建议后续迭代**：`dup`/`fcntl` 等 errno 类 contract；SMP TOML 矩阵；将 guest 日志管道接入 `diff-guest-line.sh` 的示例 CI job。

---

## 第 21 轮 — 分组提交策略文档

**目标**：落实「补提交策略」，便于审查与 bisect。

**交付物**：`docs/starryos-syscall-commit-strategy.md`（建议 5～6 个 commit 顺序、提交前自检、`VERIFY_STRICT` 与 CI 分工）。

**验证**：文档路径与仓库脚本名一致。

---

## 第 22 轮 — Catalog：`dup`

**目标**：将 `dup(2)` 纳入与 `close` 同类的 **bad fd** 工程轨迹。

**交付物**：`docs/starryos-syscall-catalog.yaml` 新增 `dup` 条目（`tests` → `dup_badfd.c`，生成器仍为 `contract_errno` 骨架）。

**验证**：`extract_starry_syscalls.py --check-catalog`。

---

## 第 23 轮 — 手写 `dup_badfd`

**目标**：Linux oracle 锚点与 `close_badfd` 对称。

**交付物**：`contract/dup_badfd.c`、`expected/dup_badfd.line`、`testcases/probe-dup_badfd-0`；`CASE dup.badfd ret=-1 errno=9`。

**验证**：`build-probes.sh`；`check_probe_coverage.py`。

---

## 第 24 轮 — `fcntl` 接上 contract

**目标**：catalog 中已有 `fcntl`，补齐最小 errno 用例。

**交付物**：`fcntl` 的 `tests` 指向 `fcntl_badfd.c`；`contract/fcntl_badfd.c`（`fcntl(-1, F_GETFD)`）、`expected/fcntl_badfd.line`、`testcases/probe-fcntl_badfd-0`。

**验证**：同第 23 轮。

---

## 第 25 轮 — `extract-case-line.sh`

**目标**：从串口日志提取首行 `CASE …`，便于对接 `diff-guest-line.sh`。

**交付物**：`test-suit/starryos/scripts/extract-case-line.sh`。

**验证**：`sh -n`；对含 `CASE` 的样例文件试跑。

---

## 第 26 轮 — `starryos-probes-ci.sh`

**目标**：一条命令跑通「无 QEMU」合并前检查。

**交付物**：`scripts/starryos-probes-ci.sh`（catalog、覆盖、`test-suit/starryos/scripts/*.sh` 的 `sh -n`、可选 `riscv64-linux-musl-gcc` 构建）。

**验证**：在仓库根执行脚本退出 0。

---

## 第 27 轮 — CI 示例片段

**目标**：降低接入 GitHub Actions 的摩擦。

**交付物**：`docs/starryos-probes-ci-example.md`（静态 job + `VERIFY_STRICT` oracle job 示例 YAML）。

**验证**：与 `commit-strategy.md` 互链。

---

## 第 28 轮 — 文档与矩阵同步

**目标**：README / 测试方法 / S0 总结反映 5 个手写 errno/IO 探针与 CI 入口。

**交付物**：更新 `probes/README.md`、`starryos-syscall-testing-method.md`、`starryos-syscall-s0-batch-summary.md`、`starryos-syscall-compat-matrix.yaml`（`dup` / `fcntl` 行）。

**验证**：`./scripts/starryos-probes-ci.sh`。

---

## 第 29 轮 — 重新生成骨架

**目标**：`dup_generated.c` 等与生成的 catalog 条目数一致。

**交付物**：`python3 scripts/gen_syscall_probes.py` → **12** 个 `*_generated.c`。

**验证**：`Wrote 12 skeleton(s)`。

---

## 第 30 轮 — 第 21–30 轮收束

**目标**：固定本批次范围。

**交付物**：本文件第 21–30 节；手写 contract **5** 个（`write_stdout`、`close_badfd`、`read_stdin_zero`、`dup_badfd`、`fcntl_badfd`）；catalog **12** 条带 `generator_hints`。

**仍建议后续迭代**：落地真实 `.github/workflows`（从示例粘贴）；`openat` 等路径类 contract；SMP 矩阵。

---
