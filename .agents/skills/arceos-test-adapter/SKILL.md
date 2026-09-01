---
name: arceos-test-adapter
description: 适配或修复 ArceOS 测试用例以通过 `cargo xtask arceos test qemu`。当用户提到新增或修改 `test-suit/arceos` 下的测试、补齐 `qemu-*.toml`、修正成功或失败匹配规则，或让某个 ArceOS 测试在任务工具中正确通过或正确失败时，使用此技能。
---

# ArceOS 测试适配

适配 `test-suit/arceos` 下的测试到 `cargo xtask arceos test qemu`。

## 测试分层

先确认测试实际需要的能力：算法、数据结构、状态机、协议解析和错误转换留在普通
`#[test]`；真实调度、阻塞/唤醒、IPI、IRQ、timer、SMP、affinity、上下文切换和目标指令
使用本 suite 的 QEMU 测试。`ax-task`、`ax-runtime` 等 ArceOS 启动依赖库不创建独立
axtest target，也不得在 std 测试中用 fake runtime 代替真实运行时。

用例的目标风险、必要性、缺陷敏感度与跨层去重先按
[`test-quality`](../test-quality/SKILL.md) 判断；本技能只补充 ArceOS QEMU 的发现、配置和运行契约。

上层 Starry kernel、Axvisor 和板卡测试包才直接持有 `axtest` 依赖，并通过
`cargo xtask ktest qemu` 或 `cargo xtask ktest board` 运行。旧的目录式 `axtest` 组不由本
命令发现；选择 `--test-group axtest` 时应转用 `ktest`。

宿主允许列表和纯模型测试规则见 [`update-std-tests`](../update-std-tests/SKILL.md)，
Starry/板卡边界和目录契约见 [`starry-test-suit`](../starry-test-suit/SKILL.md)。

## 工作方式

1. 先读目标测试的 manifest、实现入口、现有 `build-*.toml` 与 `qemu-*.toml`，不要盲目复制别的目录。
2. 再参考最接近的现有测试目录，复用必要配置，但按当前测试的实际行为改写。
3. 判断测试属于 Rust、C、generic 还是 board 流程，并使用对应发现器；不要为了复用配置改变运行时边界。
4. 改完后运行最小的 `cargo xtask arceos test qemu ...` 命令验证。

## 必查项

- Rust suite：在 `test-suit/arceos/rust` 同步 feature、模块、runner、`SELECTED_TESTS`、axbuild 可发现列表和所需 `build-*/qemu-*` 配置。
- C suite：维护 C 源码/头文件、Makefile 或 `test_cmd`，并同步 C feature 列表和成功/失败匹配。
- generic suite：按共享发现器提供 build wrapper、package manifest 和 runtime config，不假设存在 `src/main.rs`。
- board case：使用 `board-*.toml` 与目标 build 配置，通过 `cargo xtask ktest board` 验证，不纳入 QEMU 批次。
- 使用 `edition.workspace = true`；当前发现器不使用 `.axconfig.toml` 或 `.qemu.toml` 作为主要契约。
- `main.rs` 中若使用 `no_mangle`，按当前仓库风格写成 `unsafe(no_mangle)`。
- 清理无关旧产物，如 `*.out`、`*.bin`、`*.elf`、`test_cmd`。

## QEMU 输出匹配规则

- `success_regex` 必须按代码成功路径里实际打印的稳定字符串填写，并核对最终 runner 装配后的正则；Rust feature case 可能覆盖 TOML 默认值。
- 不要沿用占位字符串，不要猜测，不要默认写 `to install packages.`。
- 先从 `src/main.rs`、被调用函数和已有成功日志里找“测试成功结束时一定会出现”的输出。
- 优先选择唯一、完整、稳定的成功提示，例如 `Memory tests run OK!`、`Task yielding tests run OK!`。
- 如果代码成功路径没有明确成功提示，先在测试代码中补一条清晰且稳定的成功输出，再回填到 `success_regex`。
- 普通成功用例可使用 `(?i)\bpanic(?:ked)?\b` 作为失败匹配；预期 panic、page fault、lockdep、stack-guard 或诊断用例必须使用对应专用规则，不能用宽泛正则伪造成功。

## 适配建议

- 新增测试时，优先复制最接近的 `qemu-*.toml` 与 build 配置，再根据当前测试修正。
- 不同架构的 `success_regex` 应与该测试真实输出一致；如果成功输出跨架构相同，可以保持一致。
- 不要为了“让测试通过”去放宽正则到过于宽泛的内容。
- 失败检测要确保内核恐慌会使 `xtask` 以非零状态码退出。

## 验证

- 优先运行最小验证命令，例如 `cargo xtask arceos test qemu --test-group rust --test-case <case> --target <target-triple>`。
- 确认 QEMU 实际启动、代码打印稳定成功 marker、runner 输出 `ok: <case>`，并且 panic/失败 marker 使外层命令返回非零。
- 使用 `cargo xtask arceos test qemu --list` 检查新 case 已被发现；需要 axtest target 时改用 `cargo xtask ktest qemu`。
- 确认故意触发内核恐慌或真实失败时，`ostool` 或 `xtask` 能命中 `fail_regex` 并以非零状态码退出。
- 如果有编译警告且与当前改动相关，一并修掉。
