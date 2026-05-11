# `impl-simple-traits` 技术文档

> 路径：`components/crate_interface/test_crates/impl-simple-traits`
> 类型：库 crate / `crate_interface` 稳定测试矩阵的实现端资产
> 工作区定位：`components/crate_interface/test_crates` 独立测试工作区中的实现侧样例库
> 版本：`0.1.0`
> 文档依据：`Cargo.toml`、`src/lib.rs`、`components/crate_interface/README.md`、`components/crate_interface/Cargo.toml`、仓库根 `Cargo.toml`、`components/crate_interface/test_crates/Cargo.toml`、`components/crate_interface/test_crates/test-simple/src/main.rs`、`components/crate_interface/test_crates/run_tests.sh`、`components/crate_interface/.github/workflows/ci.yml`

`impl-simple-traits` 的真实职责很单一：为 `define-simple-traits` 中定义的接口提供一组确定性实现，并通过 `#[impl_interface]` 把这些实现导出为可链接的符号，供 `test-simple` 在最终二进制里调用。它不是 ArceOS、StarryOS 或 Axvisor 的正式运行时组件，而是 `crate_interface` 多 crate 稳定测试矩阵中的“实现端”测试资产。

## 1. 架构设计

### 1.1 真实定位

从工作区组织上看，这个 crate 被放在 `components/crate_interface/test_crates` 子工作区中。`components/crate_interface/Cargo.toml` 又显式把 `test_crates` 排除在 `crate_interface` 包内工作区之外，而仓库根 `Cargo.toml` 再把它纳入总工作区统一构建。这说明它的定位不是 `crate_interface` 库本体的一部分，而是与库本体并行维护的回归测试资产。

它在测试矩阵里的职责是“实现端”而不是“定义端”或“消费端”：

- `define-simple-traits` 负责定义接口。
- `impl-simple-traits` 负责提供实现并导出符号。
- `test-simple` 负责把定义侧与实现侧链接到一起并做断言。

### 1.2 实现矩阵

`src/lib.rs` 中的 4 个公开实现类型分别覆盖 `crate_interface` 稳定路径上的 4 个关键能力：

| 实现类型 | 对应接口 | 关键行为 | 主要验证点 |
| --- | --- | --- | --- |
| `SimpleImpl` | `SimpleIf` | 返回 `12345`、`a * b + 10`、`"SimpleImpl"` | 最基础的跨 crate 定义/实现/调用分离 |
| `NamespacedImpl` | `NamespacedIf` | 返回 `999` | `namespace` 参与符号隔离 |
| `CallerImpl` | `CallerIf` | `add_one(x) = x + 1`，`multiply(a, b) = a * b` | `gen_caller` 生成的辅助函数与 `call_interface!` 命中同一实现 |
| `AdvancedImpl` | `AdvancedIf` | `process(input) = input * 2 + 100` | `namespace + gen_caller` 的组合路径 |

这些返回值都刻意保持简单、稳定、可一眼识别，目的不是表达业务语义，而是提升测试可观测性。

### 1.3 符号导出模型

这个 crate 的核心不是“定义了几个结构体”，而是“每个 `impl` 都带着 `#[impl_interface]`”。根据 `crate_interface` README 的设计，`#[impl_interface]` 会把 trait 方法导出成统一的符号名，调用方并不直接依赖实现类型本身，而是依赖这些导出符号。

因此这里的公开类型主要承担两种职责：

- 作为 `#[impl_interface]` 的实现载体。
- 作为 `test-simple` 中的链接锚点，确保实现 crate 被最终二进制显式带入。

`test-simple/src/main.rs` 里专门通过 `std::any::type_name::<...>()` 引用这些类型，目的不是实例化对象，而是防止链接阶段把实现侧代码优化掉。

### 1.4 与正式运行时组件的边界

这个 crate 有几个边界必须明确：

- 它不是系统主线可复用的接口实现库。
- 它不维护状态、不做初始化、不承载业务逻辑。
- 它默认每个接口在同一最终二进制里只有一份实现。
- 它的实现值首先服务测试辨识度，而不是功能正确性之外的含义。

## 2. 核心功能

### 2.1 主要能力

- 为稳定路径的接口样例导出强符号实现。
- 覆盖基础调用、命名空间调用、`gen_caller` 调用和组合路径。
- 让 `test-simple` 能在 stable Rust 下验证真实的跨 crate 链接闭环。
- 作为 `crate_interface` “实现端可以独立存在”的回归样本。

### 2.2 调用闭环

这个 crate 在测试矩阵中的调用链可以概括为：

`define-simple-traits` 定义 trait  
`impl-simple-traits` 用 `#[impl_interface]` 导出实现符号  
`test-simple` 引用实现类型以确保链接  
`call_interface!` 与生成的 caller 命中这些导出符号

真正被验证的是链接期符号绑定，而不是对象方法派发。

### 2.3 最关键的边界澄清

`impl-simple-traits` 不是“供运行时直接依赖的一组接口实现”，而是 `crate_interface` 稳定测试矩阵中的实现端测试资产；这里公开的 `SimpleImpl`、`CallerImpl` 等类型本质上是符号导出和链接观测的载体。

## 3. 依赖关系

### 3.1 直接依赖

| 依赖 | 作用 |
| --- | --- |
| `crate_interface` | 提供 `#[impl_interface]` 过程宏 |
| `define-simple-traits` | 提供待实现的接口定义 |

### 3.2 测试关系

| 角色 | 对象 | 说明 |
| --- | --- | --- |
| 定义侧 | `define-simple-traits` | 负责声明接口与可选 caller |
| 实现侧 | `impl-simple-traits` | 负责导出实现符号 |
| 消费侧 | `test-simple` | 负责链接实现并断言结果 |
| 脚本入口 | `run_tests.sh simple` | 稳定路径的一键执行入口 |
| CI 入口 | `cargo run -p test-simple` | 在 `crate_interface` CI 中持续回归 |

## 4. 开发指南

### 4.1 什么时候应该修改它

只有在你想扩展 `crate_interface` 稳定多 crate 路径的测试覆盖面时，才应该改这个 crate，例如：

- 为定义侧新增了一个新的稳定接口样例。
- 需要补齐新的 `namespace` 或 `gen_caller` 组合场景。
- 需要把返回值改得更容易从断言里区分出“到底命中了哪个实现”。

### 4.2 修改时的约束

- 如果新增或改动接口，要同步更新 `define-simple-traits` 和 `test-simple`。
- `namespace` 参数必须与定义侧完全一致。
- 不要在同一最终二进制里引入同一接口的第二份实现，否则会导致符号冲突。
- 公开实现类型不要轻易改成私有，因为测试侧需要显式引用它们作为链接锚点。

### 4.3 不应该在这里做的事

- 不要把正式业务逻辑塞进这个 crate。
- 不要把它当作可复用运行时后端。
- 不要在这里测试弱默认实现回退；那属于 `impl-weak-traits` / `impl-weak-partial` 矩阵。

## 5. 测试策略

### 5.1 当前测试方式

这个 crate 自己没有独立的 `tests/`，它的验证完全依赖于更外层的多 crate 测试闭环：

- `cargo run -p test-simple`
- `./run_tests.sh simple`
- `crate_interface` CI 中的 `Multi-crate test (without weak_default)`

### 5.2 它在整体测试体系中的角色

`components/crate_interface/tests/test_crate_interface.rs` 负责验证过程宏的基础能力，而 `impl-simple-traits` 则负责把“定义在一个 crate、实现在另一个 crate、调用发生在第三个 crate”这条真实使用路径固定下来。两者互补，但职责不同：

- 单 crate 单元测试偏向宏展开和基本语义。
- 这个 crate 配合 `test-simple` 验证真实链接闭环里的实现端行为。

### 5.3 关注的风险点

- 若消费侧不再显式引用实现类型，链接器可能裁掉实现侧代码。
- 若断言值与默认值或其他场景过于接近，失败时难以定位。
- 若有人误以为它是正式实现库并在主线代码复用，会把测试资产和运行时组件混淆。

## 6. 跨项目定位

| 项目 | 与本 crate 的关系 | 实际意义 |
| --- | --- | --- |
| ArceOS | `os/arceos/modules/axlog`、`ax-runtime`、`ax-task`、`ax-driver` 等真实模块直接使用 `crate_interface`，但不会链接本 crate | 本 crate 间接保护“定义端与实现端分离”的底层语义不回退 |
| StarryOS | 当前源码下未见 `os/StarryOS` 直接引用 `crate_interface` | 本 crate 对 StarryOS 的价值是仓库级基础设施回归，而不是运行时依赖 |
| Axvisor | `components/axvisor_api` 把 `crate_interface` 封装成 API 宏能力，但不会直接消费本 crate | 本 crate 为 Axvisor 相关封装提供底层实现端语义的回归样本 |

## 7. 总结

`impl-simple-traits` 的价值不在于它“实现了几个 trait”，而在于它把 `crate_interface` 最基础、最常用的实现端路径钉成了一个可重复链接、可稳定断言的测试样例。只要它与 `test-simple` 的闭环仍然成立，`crate_interface` 在 stable Rust 下的多 crate 实现模型就仍然成立。
