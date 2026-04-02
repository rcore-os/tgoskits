# StarryOS Syscall 工程化 S0 批次总结纪要

## 范围

本批次对应路线中的 **S0-2**、**S0-3** 与 **S0-5** 最小落地；并已补充 **S0-1 方法说明** 与 **S0-4 矩阵骨架**（见下文链接）。**S0-6**：已提供 **SMP2 + 全 contract guest 串口 vs oracle** 矩阵脚本（`run-smp2-guest-matrix.sh`）；**S0-7 Review** 仍待迭代。

## 交付物清单

| 类别 | 路径 | 说明 |
|------|------|------|
| 分发表提取 | `scripts/extract_starry_syscalls.py` | 解析 `handle_syscall` 的 `match`，输出 JSON |
| 机器可读分发表 | `docs/starryos-syscall-dispatch.json` | 当前约 210 条 syscall 条目（含分区注释与 cfg） |
| Catalog 种子 | `docs/starryos-syscall-catalog.yaml` | 16 个高优先级 syscall 元数据（含 `getcwd` / `unlink` / `pipe2` / `clock_gettime` 等） |
| 探针生成器 | `scripts/gen_syscall_probes.py` | 从 catalog 生成 `*_generated.c` |
| 手写 contract | `test-suit/starryos/probes/contract/*.c` | 含 `openat`/`ioctl`/`lseek` errno 类及 `read`/`write` 零长度等 |
| 日常用法 | `docs/starryos-probes-daily.md` | 本地检查、oracle、QEMU、日志比对、SMP |
| SMP QEMU | `test-suit/starryos/qemu-riscv64-smp2.toml`、`run-starry-probe-qemu-smp2.sh` | `-smp 2`；`cargo xtask starry test qemu --qemu-config …` |
| 期望 oracle 行 | `test-suit/starryos/probes/expected/*.line` | `verify-oracle` / `verify-oracle-all` |
| 构建/差分脚本 | `build-probes.sh`、`run-diff-probes.sh`、`list-contract-probes.sh`、`diff-guest-line.sh`、`extract-case-lines.sh`、`diff-guest-cases.sh`、`run-starry-probe-qemu.sh`、`run-starry-probe-qemu-smp2.sh`、`run-smp2-guest-matrix.sh` | oracle / 单行与多行 `CASE` 集合比对 / QEMU / SMP2 矩阵 |
| 覆盖检查 | `scripts/check_probe_coverage.py` | catalog `tests:` 路径存在性 |
| 矩阵一致性 | `scripts/check_compat_matrix.py` | `parity: partial|aligned` 行对应 `contract/*.c` 与 `expected/*` |
| 基准 rootfs | `test-suit/starryos/scripts/ensure-starry-base-rootfs.sh` | 缺盘时自动 `cargo xtask starry rootfs --arch riscv64`；矩阵与 `prepare-rootfs-with-probe` 共用 |
| 镜像注入 | `prepare-rootfs-with-probe.sh`、`prepare-rootfs-with-write_stdout-probe.sh` | 通用注入 + `write_stdout` 兼容路径 |
| QEMU 用例 | `test-suit/starryos/testcases/probe-*-0` | `shell_init_cmd` 多行脚本 |
| 测试方法 | `docs/starryos-syscall-testing-method.md` | 分层与扩展清单 |
| 兼容矩阵骨架 | `docs/starryos-syscall-compat-matrix.yaml` | 与 Linux oracle 对齐结论（待填） |
| 迭代纪要 | `docs/starryos-syscall-progress-rounds.md` | 多轮交付记录 |
| 提交策略 | `docs/starryos-syscall-commit-strategy.md` | 分组 commit / PR 建议 |
| CI 示例 | `docs/starryos-probes-ci-example.md` | GitHub Actions 片段 |
| SMP2 矩阵 CI | `.github/workflows/starryos-probes-smp2-matrix.yml` | 仅 `workflow_dispatch` + nightly；跑 `run-smp2-guest-matrix.sh` |
| 本地 CI | `scripts/starryos-probes-ci.sh` | 静态检查 + 可选交叉编译 |
| xtask 扩展 | `scripts/axbuild`：`starry test qemu --test-disk-image` | 指定 ext4 基准盘用于单次测试的临时副本 |

## 验证命令速查

```sh
# 分发表 + catalog 一致性
python3 scripts/extract_starry_syscalls.py --check-catalog docs/starryos-syscall-catalog.yaml
python3 scripts/check_probe_coverage.py
./scripts/starryos-probes-ci.sh

# 重新生成分发表 JSON
python3 scripts/extract_starry_syscalls.py --out-json docs/starryos-syscall-dispatch.json

# 生成探针骨架
python3 scripts/gen_syscall_probes.py

# 交叉编译 probe（需 riscv64-linux-musl-gcc）
CC=riscv64-linux-musl-gcc test-suit/starryos/scripts/build-probes.sh

# Linux oracle（需 qemu-riscv64 / qemu-user）
test-suit/starryos/scripts/run-diff-probes.sh verify-oracle
test-suit/starryos/scripts/run-diff-probes.sh verify-oracle-all
VERIFY_STRICT=1 test-suit/starryos/scripts/run-diff-probes.sh verify-oracle-all  # 无 qemu-user 时退出码 2

# StarryOS QEMU（需先 rootfs + 注入脚本）
cargo xtask starry rootfs --arch riscv64
./test-suit/starryos/scripts/prepare-rootfs-with-write_stdout-probe.sh
cargo xtask starry test qemu --target riscv64 \
  --test-disk-image target/riscv64gc-unknown-none-elf/rootfs-riscv64-probe.img \
  --shell-init-cmd test-suit/starryos/testcases/probe-write_stdout-0 \
  --timeout 120
```

## 依赖与环境

- **riscv64-linux-musl-gcc**：构建 guest 静态 ELF。
- **qemu-riscv64**（user-mode）：`verify-oracle`；未安装时脚本会 SKIP。
- **e2fsprogs（debugfs）**：向 ext4 rootfs 注入 ELF。
- **PyYAML**：`extract_starry_syscalls.py --check-catalog` 与 `gen_syscall_probes.py`。

## 已知限制与后续工作

- 默认 `cargo starry test qemu --target riscv64` 行为未改；probe 回归依赖 **`--test-disk-image`** 与注入镜像。
- **S0-6**：**`run-smp2-guest-matrix.sh`** 覆盖 `list-contract-probes` 全量；竞态敏感项（如 `futex` / `ppoll`）仍勿单独依赖固定 `expected/*.line`。
- **S0-1 / S0-4**：已有方法与矩阵骨架文档；全文套件与矩阵逐 syscall 填全仍待迭代。
- **S0-7**：阶段 Review 仍待单独安排。
- 差分自动化（oracle 输出 vs guest 输出逐行比对）可在后续接 `run-diff-probes.sh` 扩展。

## axbuild 变更摘要

- `prepare_test_qemu_config(..., base_disk_image: Option<&Path>)`：允许测试使用预置镜像（如含 probe 的 rootfs）作为**复制源**，再生成临时测试盘。
- CLI：`cargo xtask starry test qemu --test-disk-image <path>`。
- **单测补充**：`command_parses_test_qemu_with_test_disk_image`；`prepare_test_qemu_config_copies_from_custom_base_disk_image`（确认临时盘内容来自自定义基准镜像）；`prepare_test_qemu_config_errors_when_custom_base_disk_missing`。

## 脚本注意事项

- `test-suit/starryos/scripts/prepare-rootfs-with-write_stdout-probe.sh` 位于 `test-suit/starryos/scripts/`，`WS` 需 **`$(dirname "$0")/../../.."`** 才能回到仓库根目录（勿误用 `../..` 停在 `test-suit`）。

---

*文档生成对应仓库批次实现，便于审计与交接。*
