# `define-simple-traits` 技术文档

> 路径：`components/crate_interface/test_crates/define-simple-traits`
> 类型：库 crate / `crate_interface` 多 crate 测试矩阵中的定义端测试资产
> Rust 通道：stable
> 发布属性：`publish = false`
> 文档依据：`Cargo.toml`、`src/lib.rs`、`components/crate_interface/README.md`、`components/crate_interface/Cargo.toml`、仓库根 `Cargo.toml`、`components/crate_interface/test_crates/Cargo.toml`、`components/crate_interface/test_crates/impl-simple-traits/src/lib.rs`、`components/crate_interface/test_crates/test-simple/src/main.rs`、`components/crate_interface/test_crates/run_tests.sh`

`define-simple-traits` 位于 `components/crate_interface/test_crates` 独立工作区中，在 `components/crate_interface/Cargo.toml` 和仓库根 `Cargo.toml` 里都被显式排除，不参与主工作区的常规构建与发布。它的职责不是给 ArceOS、StarryOS 或 Axvisor 提供正式运行时接口，而是为 `crate_interface` 提供稳定版多 crate 验收测试中的“定义端”样例。

## 1. 真实定位与架构设计

### 1.1 为什么单独做成定义端 crate

`crate_interface` README 的核心模型是“在一个 crate 中定义接口、在另一个 crate 中实现、在第三个 crate 中调用”。`define-simple-traits` 就是这条链路里的第一段：

| 角色 | crate | 真实职责 |
| --- | --- | --- |
| 定义端 | `define-simple-traits` | 用 `#[def_interface]` 声明接口和可选调用辅助函数 |
| 实现端 | `impl-simple-traits` | 用 `#[impl_interface]` 为接口导出实现符号 |
| 调用与验收端 | `test-simple` | 把定义端与实现端链接进最终二进制，并执行断言 |

这种拆分不是目录偏好，而是测试语义的一部分。只有把定义、实现、调用拆到不同 crate，才能真实验证 README 所描述的跨 crate 接口模型是否仍然成立。

### 1.2 接口矩阵覆盖了哪些能力

`src/lib.rs` 只定义了四组 trait，但它们分别固定住了稳定版主路径最关键的四个宏分支：

| 接口 | 宏配置 | 覆盖目标 |
| --- | --- | --- |
| `SimpleIf` | `#[def_interface]` | 验证最基础的跨 crate 定义、实现和调用路径 |
| `NamespacedIf` | `namespace = SimpleNs` | 验证接口区分依赖命名空间而非模块路径 |
| `CallerIf` | `gen_caller` | 验证生成的辅助调用函数与 `call_interface!` 一致 |
| `AdvancedIf` | `gen_caller, namespace = AdvancedNs` | 验证“命名空间 + 调用辅助函数”组合路径 |

这些接口都刻意保持极简：只有无接收者的关联函数，没有泛型、没有状态、没有默认实现。这正对应 `crate_interface` README 中对接口形态的约束。

### 1.3 这份定义集刻意不承担什么

这个 crate 有意保持“定义薄、行为少、断言强”的风格：

- 不提供默认实现，不覆盖 `weak_default` 分支。
- 不持有运行时状态，也不承担初始化、注册或资源管理。
- 不模拟真实业务协议，只提供足够暴露符号命名和调用约定问题的样例。
- 不追求外部 API 稳定性；接口名字和返回值首先服务测试覆盖面。

## 2. 核心功能

`define-simple-traits` 的核心功能不是执行业务逻辑，而是定义一组可回归的“跨 crate 链接协议样本”。它具体承担以下作用：

- 为 `crate_interface` stable 路径提供不含默认实现的基线接口集合。
- 把 `namespace` 与 `gen_caller` 两个常用能力固定成长期可回归的测试面。
- 让 `test-simple` 同时验证两种调用方式：
  - `call_interface!(Trait::method, ...)`
  - `gen_caller` 生成的普通函数，如 `add_one()`、`multiply()`、`process()`
- 为 `impl-simple-traits` 提供清晰、最小化、易断言的实现目标。

从测试视角看，这个 crate 定义的是符号命名和调用约定，不是运行时代码路径。

## 3. 依赖关系

### 3.1 直接依赖

| 依赖 | 用途 |
| --- | --- |
| `crate_interface` | 提供 `#[def_interface]` 宏和调用侧约定 |

它没有其他业务依赖，也没有启用 `weak_default`，因此能够作为 stable Rust 测试矩阵的一部分长期存在。

### 3.2 与兄弟 crate 的协作关系

- `impl-simple-traits` 依赖本 crate 暴露的 trait，并为每个方法导出实现。
- `test-simple` 同时依赖定义端和实现端，通过把实现类型引入最终二进制来触发链接，再执行运行时断言。
- `run_tests.sh simple` 把这条链路包装成稳定版多 crate 验收测试入口。
- `components/crate_interface/tests/test_crate_interface.rs` 提供的是同测试 crate 内的宏能力冒烟覆盖，而 `define-simple-traits` 所在工作区补上了真正的多 crate 链接验证。

## 4. 开发指南

只有在你要扩大 `crate_interface` 稳定版测试矩阵时，才应该修改这个 crate。典型场景包括新增命名空间组合、新增 `gen_caller` 形态，或新增更容易暴露链接回归的签名。

修改时建议遵守以下约束：

- 保持所有方法为无接收者关联函数，不引入 `self`、泛型参数或生命周期参数。
- 不要把默认实现塞进这里；涉及默认行为的语义应放到 `define-weak-traits`。
- 新增或改名接口后，必须同步更新 `impl-simple-traits` 与 `test-simple`。
- 如果新接口名可能与现有样例或未来样例冲突，优先显式添加 `namespace`。
- 返回值和参数应尽量便于肉眼判断，不要把样例复杂度堆在业务逻辑上。

## 5. 测试策略

这个 crate 自身没有独立单元测试；它的正确性通过最终二进制来验证：

- `cargo run --bin test-simple`
- `./run_tests.sh simple`

当前测试重点包括：

- 定义端、实现端、调用端拆成不同 crate 后，符号是否还能正确绑定。
- `namespace` 是否真的参与接口区分。
- `gen_caller` 生成函数与 `call_interface!` 的行为是否一致。
- 多次重复调用是否稳定命中同一实现。

高风险改动主要有两类：一类是定义变了但实现或断言没有同步更新，另一类是误把测试样例当正式接口包来维护，导致接口演化方向被业务需求绑架。

## 6. 跨项目定位

在这个仓库里，`crate_interface` 的正式用法出现在真实组件中，例如：

- ArceOS 侧：`ax-log` 定义 `LogIf`，`ax-runtime` 提供实现；`kernel_guard` 定义 `KernelGuardIf`，`ax-task` 提供实现。
- 平台与组件侧：`axplat` 对 `impl_interface` 做了平台封装。
- Axvisor 侧：`axvisor_api` 重新导出 `crate_interface` 相关宏接口。

`define-simple-traits` 与这些正式组件没有直接链接关系，也不是它们的公共 API 依赖。它在跨项目层面的真实定位，是作为 `crate_interface` 回归测试的定义端样本，间接保护这些正式组件所依赖的宏展开、符号命名和跨 crate 调用语义。

对 StarryOS 而言，当前也没有直接链接这个测试 crate；它最多只会通过共享基础设施间接受益于这类回归验证。

## 7. 最关键的边界澄清

`define-simple-traits` 是 `crate_interface` stable 测试矩阵中的定义端测试资产，不是任何子系统的正式运行时接口 crate；这里的 trait 只是测试协议样本，不应被当作可长期复用的生产 API 合约。
