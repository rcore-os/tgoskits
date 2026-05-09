# `define-weak-traits` 技术文档

> 路径：`components/crate_interface/test_crates/define-weak-traits`
> 类型：库 crate / `crate_interface` 多 crate 测试矩阵中的定义端测试资产
> Rust 通道：nightly
> 发布属性：`publish = false`
> 关键前提：启用 `crate_interface` 的 `weak_default` 特性，并在 crate 根使用 `#![feature(linkage)]`
> 文档依据：`Cargo.toml`、`src/lib.rs`、`components/crate_interface/README.md`、`components/crate_interface/Cargo.toml`、仓库根 `Cargo.toml`、`components/crate_interface/test_crates/Cargo.toml`、`components/crate_interface/test_crates/impl-weak-traits/src/lib.rs`、`components/crate_interface/test_crates/impl-weak-partial/src/lib.rs`、`components/crate_interface/test_crates/test-weak/src/main.rs`、`components/crate_interface/test_crates/test-weak-partial/src/main.rs`、`components/crate_interface/test_crates/run_tests.sh`

`define-weak-traits` 是 `crate_interface` `weak_default` 路径的定义端样例库。它的职责不是给系统组件提供“正式默认实现”，而是为链接期弱符号语义建立可重复执行的测试基线：定义端给出默认方法，后续实现端可以选择覆盖全部、覆盖部分，或者只实现必需方法，再由最终二进制验证链接器到底选中了哪一份实现。

和 `define-simple-traits` 一样，它位于独立的 `test_crates` 工作区中，被 `components/crate_interface/Cargo.toml` 和仓库根 `Cargo.toml` 双重排除，不参与主工作区常规构建，也不参与正式运行时发布。

## 1. 真实定位与架构设计

### 1.1 它在测试矩阵里的位置

`weak_default` 测的不是普通函数调用，而是“默认实现被编译为弱符号后，最终链接结果是否正确”。因此这个 crate 充当的是定义端，必须和实现端、最终验收端协同工作：

| 角色 | crate | 真实职责 |
| --- | --- | --- |
| 定义端 | `define-weak-traits` | 用 `#[def_interface]` 定义带默认实现的 trait |
| 覆盖实现端 | `impl-weak-traits` | 提供强符号覆盖样例，验证覆盖路径 |
| 部分实现端 | `impl-weak-partial` | 只实现必需方法，验证默认回退路径 |
| 调用与验收端 | `test-weak` | 链接覆盖实现端并断言强符号优先 |
| 调用与验收端 | `test-weak-partial` | 链接部分实现端并断言弱符号回退 |

`components/crate_interface/tests/test_weak_default.rs` 只能提供同测试 crate 内的功能冒烟，而这套 `test_crates` 工作区才承担真正的多 crate 链接行为验证。

### 1.2 接口矩阵覆盖了哪些风险点

`src/lib.rs` 定义了五组 trait，对应 `weak_default` 最容易出问题的几条路径：

| 接口 | 关键特征 | 覆盖目标 |
| --- | --- | --- |
| `WeakDefaultIf` | 必需方法 + 若干默认方法 | 验证强符号覆盖与默认回退的基本语义 |
| `AllDefaultIf` | 所有方法都有默认实现 | 验证“几乎空实现”场景仍可工作 |
| `NamespacedWeakIf` | `namespace = WeakNs` | 验证命名空间与弱默认语义并存 |
| `CallerWeakIf` | `gen_caller` | 验证生成的辅助调用函数与弱默认并存 |
| `SelfRefIf` | 默认实现内部直接或间接引用 `Self::method` | 验证代理重写和最终绑定是否正确 |

其中 `SelfRefIf` 是这组样例里最关键的一项，因为它不仅测默认实现本身，还测默认实现内部的 `Self::base_value()` 和 `let f = Self::transform` 这类引用，在强符号覆盖和默认回退两种情况下是否都能指向正确目标。

### 1.3 `weak_default` 在这里测的到底是什么

结合 README 和兄弟 crate 的源码，可以把这条语义链概括为：

1. 定义端把带默认实现的方法生成为弱符号。
2. 实现端如果覆写某个方法，就导出同名强符号。
3. 最终二进制把定义端与实现端一起链接后，由链接器决定使用强符号还是弱符号。

因此，`define-weak-traits` 的核心价值不是“提供默认逻辑”，而是“定义一组能暴露链接期选择结果的默认逻辑样本”。

## 2. 核心功能

`define-weak-traits` 负责把 `crate_interface` 的 nightly 弱默认能力固化成一组可回归的定义端样例。它具体承担以下功能：

- 为 `weak_default` 分支提供带默认实现的接口定义集合。
- 把“完整覆盖”“部分覆盖”“全默认”“命名空间”“生成调用辅助函数”“默认实现内部自引用”这些关键分支固定成长期样例。
- 为 `impl-weak-traits` 和 `impl-weak-partial` 提供共同的定义端输入。
- 让 `test-weak` 与 `test-weak-partial` 可以通过不同链接组合验证强弱符号优先级。

这里的默认方法都刻意设计成容易区分“默认结果”和“覆写结果”的常量或简单算式，目的是提升回归测试的可判读性，而不是模拟业务层兜底逻辑。

## 3. 依赖关系

### 3.1 直接依赖

| 依赖 | 用途 |
| --- | --- |
| `crate_interface`（启用 `weak_default`） | 提供带弱默认实现能力的 `#[def_interface]` 宏 |

与 `define-simple-traits` 不同，它显式通过路径依赖打开 `weak_default`，并要求 nightly Rust 与 `#![feature(linkage)]` 配套使用。

### 3.2 与兄弟 crate 的协作关系

- `impl-weak-traits` 提供覆盖样例，验证强符号是否优先于弱默认实现。
- `impl-weak-partial` 只实现必需方法，验证未覆写方法是否回退到默认实现。
- `test-weak` 负责把覆盖实现端链接进最终二进制，并断言覆写路径。
- `test-weak-partial` 负责把部分实现端链接进最终二进制，并断言回退路径。
- `run_tests.sh weak` 把上述两条路径打包成 nightly 多 crate 验收测试入口。

## 4. 开发指南

只有在你要扩展 `crate_interface` `weak_default` 语义覆盖面时，才应该修改这个 crate。典型场景包括新增会调用其他默认方法的默认实现、新增 `namespace` 与 `gen_caller` 的组合样例，或新增更复杂的 `Self::method` 间接引用路径。

修改时应重点遵守以下约束：

- 保持 `weak_default` 依赖配置与 `#![feature(linkage)]` 一致。
- 新增默认方法后，必须同时补齐覆盖路径和回退路径两套验证。
- 如果默认实现内部调用 `Self::foo()` 或把 `Self::foo` 当作函数值使用，要同时验证“被覆写”和“未覆写”两种情况。
- 像 `required_value()`、`required_name()`、`required_id()` 这类必需方法不要随意删除，它们是实现端注册和结果判别的重要锚点。
- 接口仍应保持 `crate_interface` README 所要求的约束，不引入带接收者的方法或泛型接口。

## 5. 测试策略

这个 crate 自身没有独立单元测试；它通过最终测试二进制验证实际链接结果：

- `cargo +nightly run --bin test-weak`
- `cargo +nightly run --bin test-weak-partial`
- `./run_tests.sh weak`

当前测试重点包括：

- 弱默认实现是否被正确生成并参与最终链接。
- 实现端导出的强符号是否能覆盖默认实现。
- 未覆写的方法是否会回退到默认实现。
- `namespace` 与 `gen_caller` 是否在弱默认场景下仍保持正确行为。
- 默认实现内部的 `Self::` 直接调用和函数值引用是否都能被正确代理。

其中一个重要分工是：`components/crate_interface/tests/test_weak_default.rs` 负责特性可用性和基础行为的冒烟验证，而 `define-weak-traits` 这套工作区负责真正的多 crate 链接回归。

高风险改动主要集中在默认实现变更后只更新了一侧验证，或误把默认方法当成正式 API 的兜底语义来扩展，这两类错误都会直接模糊测试边界。

## 6. 跨项目定位

仓库中有若干正式组件依赖 `crate_interface` 生态，例如 ArceOS 的 `ax-log`/`ax-runtime`、`kernel_guard`/`ax-task`，以及 Axvisor 侧的 `axvisor_api` 宏封装；但这些正式组件都不会直接链接 `define-weak-traits`。

因此，这个 crate 在跨项目层面的定位并不是“给主线系统提供默认实现库”，而是作为 `crate_interface` 弱默认分支的定义端回归样本，间接保护未来或现有正式组件所依赖的宏展开、符号命名和链接期选择语义。

对 StarryOS 而言，当前同样没有直接依赖这份测试定义集；它最多只会通过共享基础设施间接受益于这里的验证结果。

## 7. 最关键的边界澄清

`define-weak-traits` 不是带默认实现的正式接口库，而是 `crate_interface` nightly 测试矩阵中的定义端测试资产；这里的默认方法是给链接器行为做回归验证的测试样本，不是给运行时模块提供兜底逻辑的生产实现。
