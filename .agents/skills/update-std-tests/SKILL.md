---
name: update-std-tests
description: 审计并更新本 ArceOS 与 StarryOS 工作区的 `scripts/test/std_crates.csv`。用户提到标准库测试、允许列表、`cargo test` 验证、检查哪些软件包能通过宿主机测试、刷新测试套件，或要求向测试表格添加新软件包时使用。本技能是维护标准库测试候选列表的主要流程。
---

# 更新标准库测试列表

本技能通过比较工作区软件包与宿主机完整 `cargo test` 结果，维护 `scripts/test/std_crates.csv` 中的标准库测试允许列表。

## 测试分层边界

先按被测语义选择测试层，再审计允许列表：

- `std` 只验证算法、数据结构、状态机、协议解析、错误转换和可确定性模型。测试可以使用局部数据夹具，但不得实现或依赖假调度器、假 IRQ、假 timer、假 SMP 或假设备来证明运行时语义。
- 真实调度、阻塞/唤醒、IPI、IRQ、timer、SMP、affinity、上下文切换和目标指令必须通过 `test-suit/arceos/rust` 的 ArceOS QEMU case 验证。`ax-task`、`ax-runtime` 等启动依赖库不创建独立 axtest target。
- Starry kernel、Axvisor 和板卡专属行为使用 `cargo xtask ktest qemu` 或 `cargo xtask ktest board`；直接 axtest 依赖不会因为传递依赖而自动扩散。
- ArceOS suite 的正式入口是 `cargo xtask arceos test qemu ...`，不是 `cargo xtask test arceos`。Starry suite 使用 `cargo xtask starry test ...`。

同一 crate 可以同时拥有 std 模型测试和上层 QEMU/axtest 集成测试，但每个断言只能由最接近其真实语义的一层负责。不能用宿主机编译成功、fake runtime 或 shell prompt 替代目标运行时证据。

相关层级适配规则见 [`arceos-test-adapter`](../arceos-test-adapter/SKILL.md) 和
[`starry-test-suit`](../starry-test-suit/SKILL.md)。

## 工作流程

1. 运行审计，找出不在允许列表中的候选软件包。
2. 先询问是否添加通过测试的候选软件包；默认建议添加。
3. 再询问是否添加当前测试失败的候选软件包；只有用户明确选择才添加。
4. 用户确认后，只应用明确选择的软件包。
5. 如果只添加通过测试的软件包，使用 `cargo xtask test` 验证；增量验证使用 `cargo xtask test --since <REF>`。

## 命令

脚本位于 `.agents/skills/update-std-tests/scripts/std_test_candidates.py`。本仓库所有 Python 脚本都使用 `python3`。

输出 Markdown 格式的审计结果：

```bash
python3 .agents/skills/update-std-tests/scripts/std_test_candidates.py audit --repo-root /path/to/repo --format markdown
```

输出 JSON 格式的审计结果：

```bash
python3 .agents/skills/update-std-tests/scripts/std_test_candidates.py audit --repo-root /path/to/repo --format json
```

把软件包加入逗号分隔值文件：

```bash
python3 .agents/skills/update-std-tests/scripts/std_test_candidates.py apply --repo-root /path/to/repo --packages pkg1 pkg2 pkg3
```

只预览修改，不实际写入：

```bash
python3 .agents/skills/update-std-tests/scripts/std_test_candidates.py apply --repo-root /path/to/repo --packages pkg1 pkg2 --dry-run
```

## 征求用户确认

应用修改前始终确认。对通过测试的候选项询问“是否添加全部通过测试的软件包？”。对失败候选项询问“是否添加当前失败的软件包？可选：`all`、`ignore` 或以逗号分隔的软件包名”。

## 筛选策略

- 纳入：库软件包、只有二进制目标的示例软件包。
- 按名称排除：`tg-xtask`、`axlibc`、`arm_vcpu`、`riscv_vcpu`、`axvisor`。
- 按失败特征排除：`invalid register`、`undefined symbol: main`，这两类表明软件包不兼容宿主机。
- 测试方式：普通包可完整执行 `cargo test -p <package>`，不使用 `--no-run`；带有 `host-test`、固定 feature profile 或测试发现断言的软件包必须通过 `cargo xtask test` 的正式 profile 验证。

详细筛选逻辑见 `references/filtering.md`。

## 输出格式

候选项按下列顺序分组，并保持清楚分隔：

```text
## 通过测试的候选项（N）
- `package-name`（类型）- 路径 - 通过 cargo test

## 测试失败的候选项（N）
- `package-name`（类型）- 路径 - 错误消息

## 已排除的候选项（N）
- `package-name`（类型）- 路径 - 排除原因
```

## 验证

只添加通过测试的软件包后，建议运行：

```bash
cargo xtask test
```

如果用户选择加入已知失败软件包，明确警告允许列表包含当前失败项，整体验证可能无法通过。

应用 CSV 修改前先展示候选包、退出码和实际测试数量，确认每个条目仍是 workspace package，且不是零测试、宏装配、裸机应用或目标架构专属包；只应用明确选中的条目，并检查 `git diff`。工作区成员、目标种类、宿主测试行为或依赖发生变化时重新运行审计。

## 附带资源

- `scripts/std_test_candidates.py`：审计和应用脚本（位于 `.agents/skills/update-std-tests/scripts/`）。
- `references/filtering.md`：详细筛选策略。
