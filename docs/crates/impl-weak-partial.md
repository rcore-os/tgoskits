# `impl-weak-partial` 技术文档

> 路径：`components/crate_interface/test_crates/impl-weak-partial`
> 类型：库 crate / `crate_interface` `weak_default` 测试矩阵的实现端资产
> 工作区定位：`components/crate_interface/test_crates` 独立测试工作区中的“纯回退”样例库
> 版本：`0.1.0`
> 文档依据：`Cargo.toml`、`src/lib.rs`、`components/crate_interface/README.md`、`components/crate_interface/Cargo.toml`、仓库根 `Cargo.toml`、`components/crate_interface/test_crates/Cargo.toml`、`components/crate_interface/test_crates/define-weak-traits/src/lib.rs`、`components/crate_interface/test_crates/test-weak-partial/src/main.rs`、`components/crate_interface/tests/test_weak_default.rs`、`components/crate_interface/test_crates/run_tests.sh`、`components/crate_interface/.github/workflows/ci.yml`

`impl-weak-partial` 是 `crate_interface` 弱默认实现测试矩阵中的“实现端纯回退样例库”。它故意只实现必需方法，把带默认实现的方法留给 `define-weak-traits` 生成的弱符号去承担。它不是“不完整的运行时后端”，而是专门用来证明“实现端缺省不覆写时，默认弱符号会在最终二进制里真实接管”的测试资产。

## 1. 架构设计

### 1.1 真实定位

`crate_interface` README 和 `test_weak_default.rs` 都在强调同一件事：`weak_default` 的价值在于跨 crate 的链接期选择，而不是把默认实现当成普通的同 crate trait 默认方法来理解。`impl-weak-partial` 正是这个设计中的实现端另一半，它与 `define-weak-traits`、`test-weak-partial` 组成“默认回退”闭环。

它在测试矩阵中的职责很明确：

- `define-weak-traits` 负责定义接口并产生默认弱符号。
- `impl-weak-partial` 只实现必需方法，刻意保留空缺。
- `test-weak-partial` 负责验证这些空缺最终是否正确回退到默认实现。

### 1.2 实现矩阵

这个 crate 只有两个公开实现类型，而且设计重点不在“实现了多少”，而在“刻意没实现哪些方法”：

| 实现类型 | 对应接口 | 显式实现的方法 | 刻意不实现的方法 | 主要验证点 |
| --- | --- | --- | --- | --- |
| `PartialOnlyImpl` | `WeakDefaultIf` | `required_value`、`required_name` | `default_value`、`default_add`、`default_greeting` | 默认弱符号能否为可选方法兜底 |
| `SelfRefPartialImpl` | `SelfRefIf` | `required_id` | `base_value`、`transform` 以及所有衍生默认方法 | 默认实现中的 `Self::` 直接/间接引用能否继续走默认路径 |

从测试设计上说，`impl-weak-partial` 的“缺失”不是缺陷，而是它最重要的行为定义。

### 1.3 为什么必须与 `impl-weak-traits` 分开

`src/lib.rs` 顶部注释已经点明，这个 crate 被拆成单独的包，是为了隔离弱符号机制的回退路径。原因很直接：

- `impl-weak-traits` 会为部分方法导出强符号。
- `impl-weak-partial` 想证明的是“没有这些强符号时，默认弱符号仍然可靠”。

如果两者一起链接，回退测试就会被强符号污染，无法说明问题。

### 1.4 `SelfRefPartialImpl` 的特殊价值

`SelfRefPartialImpl` 是这份样例中最容易被低估的一部分。它不覆写 `base_value()` 和 `transform()`，于是测试侧可以验证：

- `Self::base_value()` 的直接调用，是否仍然命中默认实现。
- `let f = Self::transform; f(v)` 这种把方法当值使用的间接引用，是否也仍然命中默认实现。

这说明 `crate_interface` 对默认实现内部 `Self::` 代理的处理，不只是为“强覆盖”场景服务，也必须在“完全回退”场景下成立。

### 1.5 与正式运行时组件的边界

这个 crate 的边界必须写死：

- 它不是正式后端实现库。
- 它的“少实现”是测试设计，不是功能未完成。
- 它只适合与 `test-weak-partial` 这类观测端配套使用。
- 它的返回值和行为以可观测、可断言为优先目标。

## 2. 核心功能

### 2.1 主要能力

- 为必需方法导出最小强符号实现。
- 让带默认实现的方法全部回退到定义侧的弱符号。
- 验证默认实现内部 `Self::` 直接调用与函数引用路径的纯回退行为。
- 作为 `test-weak-partial` 的实现端输入，承担弱默认矩阵中的“回退半边”。

### 2.2 调用闭环

这条测试链可以概括为：

`define-weak-traits` 定义接口并生成默认弱符号  
`impl-weak-partial` 只为必需方法导出强符号  
`test-weak-partial` 链接实现类型并执行断言  
最终观测“未覆写方法是否稳定回退到默认弱符号”

### 2.3 最关键的边界澄清

`impl-weak-partial` 不是“没写完的实现库”，而是 `crate_interface` `weak_default` 测试矩阵中专门保留方法空缺、用来证明默认回退成立的实现端测试资产。

## 3. 依赖关系

### 3.1 直接依赖

| 依赖 | 作用 |
| --- | --- |
| `crate_interface` | 提供 `#[impl_interface]` 过程宏 |
| `define-weak-traits` | 提供带默认实现的接口定义与默认弱符号来源 |

### 3.2 测试关系

| 角色 | 对象 | 说明 |
| --- | --- | --- |
| 定义侧 | `define-weak-traits` | 负责声明带默认实现的接口 |
| 实现侧 | `impl-weak-partial` | 负责导出最小实现并保留回退空缺 |
| 消费侧 | `test-weak-partial` | 负责链接并断言默认回退行为 |
| 脚本入口 | `run_tests.sh weak` | nightly 路径的一键执行入口 |
| CI 入口 | `cargo run -p test-weak-partial` | `crate_interface` CI 的多 crate 回退路径回归 |

## 4. 开发指南

### 4.1 什么时候应该修改它

只有当你想扩展“默认实现回退”这条测试面时，才应该改这里，例如：

- 在定义侧新增了一个默认方法，需要验证“不实现也能工作”。
- 新增一个包含 `Self::` 直接调用或间接引用的默认实现，需要验证纯默认路径。
- 需要调整返回值，使回退结果更容易从断言里识别出来。

### 4.2 修改时的约束

- 不要把本应测试回退的方法意外实现掉。
- 如果定义侧新增默认方法，要同步更新 `test-weak-partial` 断言。
- 不要把这个 crate 与 `impl-weak-traits` 一起链接到同一最终二进制。
- 公开实现类型不要轻易改成私有，因为测试侧需要显式引用它们作为链接锚点。

### 4.3 不应该在这里做的事

- 不要把它当成正式运行时后端。
- 不要为了“让 trait 更完整”而补齐本应留空的方法。
- 不要把强覆盖场景塞进这里；那属于 `impl-weak-traits`。

## 5. 测试策略

### 5.1 当前测试方式

这个 crate 自己没有独立的 `tests/`，而是通过 nightly 多 crate 路径被验证：

- `cargo +nightly run -p test-weak-partial`
- `./run_tests.sh weak`
- `crate_interface` CI 中的 `Multi-crate test (with weak_default, nightly only)`

### 5.2 它在整体测试体系中的角色

`components/crate_interface/tests/test_weak_default.rs` 已经提供了最小化的同 crate 回退语义验证，但 `impl-weak-partial` 配合 `test-weak-partial` 仍然不可替代，因为它额外证明了两件事：

- 跨 crate 链接下，默认回退仍然成立。
- 在与“强覆盖样例库”隔离的前提下，回退结果没有被其他实现污染。

### 5.3 关注的风险点

- 一旦误把 `impl-weak-traits` 也链接进来，这组测试会立刻失去解释力。
- 如果默认值与必需方法返回值差异不够明显，失败时很难判断命中路径。
- 若只测试直接调用、不测试函数引用形式，`Self::` 代理问题可能被漏掉。

## 6. 跨项目定位

| 项目 | 与本 crate 的关系 | 实际意义 |
| --- | --- | --- |
| ArceOS | `ax-log`、`ax-runtime`、`ax-task`、`ax-driver` 等真实模块依赖 `crate_interface`，但不会链接本 crate | 本 crate 间接保护默认回退与实现端分离这类底层语义 |
| StarryOS | 当前源码下未见 `os/StarryOS` 直接引用 `crate_interface` | 本 crate 的意义更多是仓库级基础设施回归，而不是运行时依赖 |
| Axvisor | `axvisor_api` 用宏包装 `crate_interface`，但不会直接消费本 crate | 本 crate 为 Axvisor 侧接口桥接的弱默认底层语义提供保底样例 |

## 7. 总结

`impl-weak-partial` 的价值，不在于它实现了多少方法，而在于它精确地保留了哪些空缺。正是这些空缺，让 `test-weak-partial` 能持续验证：当实现端选择“不覆写”时，`crate_interface` 生成的默认弱符号会不会在真实最终二进制里接手工作。它是实现端测试资产，不是运行时组件。
