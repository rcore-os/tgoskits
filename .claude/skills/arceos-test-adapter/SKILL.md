---
name: arceos-test-adapter
description: 适配或修复 ArceOS 测试用例以通过 `cargo xtask test arceos`。当用户提到新增或修改 `test-suit/arceos` 下的测试、补齐 `qemu-*.toml`、修正 success/fail 正则、让某个 ArceOS 测试在 xtask 中正确通过或正确失败时，使用此技能。
---

# ArceOS Test Adapter

适配 `test-suit/arceos` 下的测试到 `cargo xtask test arceos`。

## 工作方式

1. 先读目标测试的 `Cargo.toml`、`src/main.rs`、现有 `qemu-*.toml`，不要盲目复制别的目录。
2. 再参考最接近的现有测试目录，复用必要配置，但按当前测试的实际行为改写。
3. 改完后运行相关 `cargo xtask test arceos ...` 命令验证。

## 必查项

- 保持测试目录至少包含 `Cargo.toml`、`src/main.rs` 和目标架构对应的 `qemu-*.toml`。
- `.axconfig.toml` 视测试是否需要而定；缺失时先参考相邻测试。
- `.qemu.toml` 非必须；优先维护 `qemu-{arch}.toml`，只有工具链明确依赖默认配置时才补。
- `Cargo.toml` 优先使用 `edition.workspace = true`。
- `main.rs` 中若使用 `no_mangle`，按当前仓库风格写成 `unsafe(no_mangle)`。
- 清理无关旧产物，如 `*.out`、`*.bin`、`*.elf`、`test_cmd`。

## QEMU 正则规则

- `success_regex` 必须按代码成功路径里实际打印的稳定字符串填写。
- 不要沿用占位字符串，不要猜测，不要默认写 `to install packages.`。
- 先从 `src/main.rs`、被调用函数和已有成功日志里找“测试成功结束时一定会出现”的输出。
- 优先选择唯一、完整、稳定的成功提示，例如 `Memory tests run OK!`、`Task yielding tests run OK!`。
- 如果代码成功路径没有明确成功提示，先在测试代码中补一条清晰且稳定的成功输出，再回填到 `success_regex`。
- `fail_regex` 统一写成 `(?i)\bpanic(?:ked)?\b`。

## 适配建议

- 新增测试时，优先复制最接近的 `qemu-*.toml` 和 `.axconfig.toml`，再根据当前测试修正。
- 不同架构的 `success_regex` 应与该测试真实输出一致；如果成功输出跨架构相同，可以保持一致。
- 不要为了“让测试通过”去放宽正则到过于宽泛的内容。
- 失败检测要确保 panic 会使 xtask 非零退出。

## 验证

- 优先运行最小验证命令，例如 `cargo xtask test arceos --target <target-triple>`。
- 确认成功用例会输出 `ok: <package>`。
- 确认故意 panic 或真实失败时，ostool/xtask 能命中 `fail_regex` 并非零退出。
- 如果有编译警告且与当前改动相关，一并修掉。
