# StarryOS Syscall 工程 — 分组提交策略

目标：审查清晰、可 `git bisect`、文档与可执行代码不同步时仍易回滚。下列顺序从 **底层依赖 → 用户可见行为 → 纯文档**。

## 推荐提交序列（5～6 个 commit）

### 1. `chore(gitignore): ignore StarryOS probe build output`

- 仅 `.gitignore`（例如 `test-suit/starryos/probes/build-riscv64/`）。
- 无功能变更，便于单独回滚忽略规则。

### 2. `feat(axbuild): starry test qemu --test-disk-image`

- `scripts/axbuild/src/starry/mod.rs`、`rootfs.rs` 及对应单元测试。
- **不含** `test-suit/starryos` 与 syscall 文档。

### 3. `feat(starryos): syscall dispatch extract and probe tooling`

- `scripts/extract_starry_syscalls.py`
- `scripts/gen_syscall_probes.py`
- `scripts/check_probe_coverage.py`
- `scripts/starryos-probes-ci.sh`
- `docs/starryos-syscall-dispatch.json`（若团队希望 JSON **不**进版本库，可改 `.gitignore` 并仅在 CI 生成；当前策略为 **纳入** 以便离线审计）

### 4. `feat(starryos): syscall probes, scripts, and testcases`

- `docs/starryos-syscall-catalog.yaml`
- `test-suit/starryos/probes/**`（`contract/`、`expected/`、`generated/`）
- `test-suit/starryos/scripts/**`
- `test-suit/starryos/testcases/probe-*`

### 5. `docs(starryos): syscall engineering documentation`

- `docs/starryos-syscall-*.md`、`docs/starryos-syscall-compat-matrix.yaml`（若希望矩阵与代码同提交，可并入 commit 4）
- `test-suit/starryos/probes/README.md`

### 6.（可选）`docs(starryos): progress rounds and CI notes`

- `docs/starryos-syscall-progress-rounds.md`
- `docs/starryos-probes-ci-example.md` 等纯说明

## 提交前自检（最小集）

```sh
python3 scripts/extract_starry_syscalls.py --check-catalog docs/starryos-syscall-catalog.yaml
python3 scripts/check_probe_coverage.py
./scripts/starryos-probes-ci.sh
cargo fmt -p axbuild && cargo clippy -p axbuild -- -D warnings   # 若含 axbuild 变更
```

## 合并策略建议

- **单 PR**：按上述顺序多个 commit，reviewer 可按 commit 浏览。
- **拆 PR**：先合 axbuild（2），再合工具链（3），再合探针（4），最后文档（5），减少冲突面。

## 与 `VERIFY_STRICT` / QEMU

- 默认 CI 不要求安装 `qemu-riscv64`：`scripts/starryos-probes-ci.sh` 只做静态检查与可选交叉编译。
- 需要 Linux oracle 时，在独立 job 安装 `qemu-user` 并执行 `VERIFY_STRICT=1 test-suit/starryos/scripts/run-diff-probes.sh verify-oracle-all`（参见 `docs/starryos-probes-ci-example.md`）。
