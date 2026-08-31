# 标准库测试候选项筛选规则

## 概述

使用 `cargo metadata --no-deps` 枚举工作区软件包，与 `scripts/test/std_crates.csv` 比较，再依据宿主机上完整 `cargo test -p <package>` 或仓库正式 `cargo xtask test` profile 的结果分类缺失软件包。

只把宿主可确定执行的算法、数据结构、状态机、协议解析和错误转换纳入 std。依赖假调度器、假 IRQ、假 timer、假 SMP 或假设备来证明真实运行时语义的测试不属于 std，应迁移到 ArceOS QEMU、Starry/Axvisor axtest 或板卡流程。纯协议测试可以使用局部数据夹具，但不得伪造 OS/runtime 行为。

## 候选来源

- 只从当前工作区取得软件包。
- 把 `scripts/test/std_crates.csv` 视为现有允许列表的事实来源。
- 忽略逗号分隔值文件中的空行，并要求表头只有一个 `package` 字段。

## 纳入规则

- 把库软件包纳入审计候选集合。
- 把示例或只有二进制目标的软件包纳入候选集合。
- 普通包使用完整 `cargo test -p <package>`，不使用 `--no-run`；带 `host-test`、固定 feature profile 或测试发现断言的软件包使用 `cargo xtask test` 的对应 profile。

## 默认排除

- `tg-xtask`：仓库工具，不属于标准库测试套件。
- `axlibc`：只有 `staticlib` 产物，不能按普通软件包测试。
- `arm_vcpu` 和 `riscv_vcpu`：体系结构特定，不能在当前宿主机上编译。
- `axvisor`：依赖自有运行时的裸机应用。
- 明确表示宿主机不兼容的失败：
  - `invalid register`：内联汇编与宿主机不兼容；
  - `undefined symbol: main`：缺少宿主机入口点。

## 排除理由

排除这些软件包是为了避免测试套件产生伪失败：

| 类别 | 理由 | 示例 |
| --- | --- | --- |
| 仓库工具 | 不属于标准库测试套件 | `tg-xtask` |
| 体系结构特定 | 无法在宿主机编译 | `arm_vcpu`、`riscv_vcpu` |
| 构建产物 | 不是可直接测试的软件包 | `axlibc`，仅生成 `staticlib` |
| 裸机应用 | 需要自定义运行时 | `axvisor` |
| 宿主机不兼容特征 | 在宿主机测试必然失败 | `invalid register` |

## 预期分类

审计脚本应生成三类结果：

- 通过测试的候选项：在宿主机上通过 `cargo test -p <package>`；
- 测试失败的候选项：测试失败，但仍可能是有效候选，例如缺少依赖；
- 已排除的候选项：不应进入允许列表。

发生下列变化时重新运行审计：

- 工作区成员变化；
- 目标种类变化；
- 宿主机测试行为变化；
- 依赖更新。

本文件描述筛选逻辑，不记录固定允许列表。实际候选项随工作区状态变化。
