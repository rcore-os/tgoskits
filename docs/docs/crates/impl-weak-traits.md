# `impl-weak-traits` 技术文档

> 路径：`components/crate_interface/test_crates/impl-weak-traits`
> 类型：库 crate / `crate_interface` `weak_default` 测试矩阵的实现端资产
> 工作区定位：`components/crate_interface/test_crates` 独立测试工作区中的“强覆盖”样例库
> 版本：`0.1.0`
> 文档依据：`Cargo.toml`、`src/lib.rs`、`components/crate_interface/README.md`、`components/crate_interface/Cargo.toml`、仓库根 `Cargo.toml`、`components/crate_interface/test_crates/Cargo.toml`、`components/crate_interface/test_crates/define-weak-traits/src/lib.rs`、`components/crate_interface/test_crates/test-weak/src/main.rs`、`components/crate_interface/tests/test_weak_default.rs`、`components/crate_interface/test_crates/run_tests.sh`、`components/crate_interface/.github/workflows/ci.yml`

`impl-weak-traits` 是 `crate_interface` 弱默认实现测试矩阵里的“实现端强覆盖样例库”。它不是为了给业务代码提供一组带默认值的后端实现，而是为了制造一批可观察的强符号，实现 `define-weak-traits` 中的接口并压过默认弱符号，再由 `test-weak` 在最终二进制里验证链接器的真实选择结果。

## 1. 架构设计

### 1.1 真实定位

`crate_interface` README 明确指出：带默认实现的接口，其 `weak_default` 语义本质上是跨 crate 的链接期机制；定义端负责产生弱符号，实现端负责按需产生强符号，最终由链接器决定谁生效。也正因为如此，`impl-weak-traits` 必须是独立 crate，不能被误解为 `define-weak-traits` 的“附带实现”。

它在测试矩阵中的位置是：

- `define-weak-traits` 负责定义带默认实现的接口，并开启 `weak_default`。
- `impl-weak-traits` 负责提供“强覆盖”这一半的实现端样本。
- `test-weak` 负责把它们链接到一起，验证强符号覆盖、部分回退、命名空间和 caller 路径。

### 1.2 实现矩阵

`src/lib.rs` 中的 5 个公开实现类型，分别覆盖不同的强弱符号组合情形：

| 实现类型 | 对应接口 | 显式覆写的方法 | 仍使用默认弱符号的方法 | 主要验证点 |
| --- | --- | --- | --- | --- |
| `FullImpl` | `WeakDefaultIf` | `required_value`、`required_name`、`default_value`、`default_add`、`default_greeting` | 无 | 强符号全面压过弱默认 |
| `AllDefaultImpl` | `AllDefaultIf` | `method_a` | `method_b`、`method_c` | 同一接口内允许强弱并存 |
| `NamespacedWeakImpl` | `NamespacedWeakIf` | `get_id` | `get_default_multiplier` | `namespace` 与弱默认可同时成立 |
| `CallerWeakImpl` | `CallerWeakIf` | `compute` | `default_offset` | `gen_caller` 与弱默认可同时成立 |
| `SelfRefFullImpl` | `SelfRefIf` | `required_id`、`base_value`、`transform` | `derived_value`、`derived_with_offset`、`call_via_ref`、`call_twice` | 默认实现内部 `Self::` 引用能否跳到强符号 |

这里最重要的不是“实现数量多”，而是覆写粒度被故意设计成不同层次，使测试侧能同时观察到“强覆盖生效”和“未覆写方法继续回退”的混合现象。

### 1.3 强符号与弱符号的协作方式

这组样例的运行模型来自两端协作：

- `define-weak-traits` 依赖启用了 `weak_default` 特性的 `crate_interface`，并在 crate 根启用 `#![feature(linkage)]`，从而把默认实现编译成弱符号。
- `impl-weak-traits` 也启用 `#![feature(linkage)]`，再通过 `#[impl_interface]` 为显式实现的方法导出强符号。
- `test-weak` 只链接这一份实现侧样例库，因此最终二进制中的符号解析结果是可控、可断言的。

这说明它测试的重点不是算法本身，而是链接期优先级是否与 README 描述一致。

### 1.4 `SelfRefFullImpl` 的特殊价值

`SelfRefFullImpl` 是这份文档里必须重点理解的样例。它只把 `base_value()` 从 `100` 改成 `500`、把 `transform(v)` 从 `v + 1` 改成 `v * 10`，却保留 `derived_value()`、`derived_with_offset()`、`call_via_ref()`、`call_twice()` 继续使用定义侧默认实现。

因此它可以同时验证两件最容易出错的事：

- 默认实现内部的 `Self::base_value()` 直接调用，是否会落到强符号版本。
- `let f = Self::transform; f(v)` 这种把方法当成值的间接引用，是否也会落到强符号版本。

### 1.5 与 `impl-weak-partial` 的边界

`impl-weak-traits` 与 `impl-weak-partial` 都属于实现端测试资产，但职责相反：

- `impl-weak-traits` 制造“强覆盖”场景。
- `impl-weak-partial` 制造“纯回退”场景。

如果把两者一起链接到同一最终二进制，部分回退路径会被这里导出的强符号污染，测试结果就失去解释力。

## 2. 核心功能

### 2.1 主要能力

- 为带默认实现的接口导出一组可观测的强符号实现。
- 验证“全部覆写”“部分覆写”“仅覆写必需方法”三种强覆盖粒度。
- 验证 `namespace`、`gen_caller` 和 `Self::` 代理在弱默认语义下依旧成立。
- 作为 `test-weak` 的实现端输入，承担弱默认矩阵中的“强覆盖半边”。

### 2.2 调用闭环

这条测试链可以概括为：

`define-weak-traits` 定义接口并生成默认弱符号  
`impl-weak-traits` 用 `#[impl_interface]` 导出部分或全部强符号  
`test-weak` 链接实现类型并执行断言  
最终观测“哪些方法命中强符号、哪些方法继续落回默认弱符号”

### 2.3 最关键的边界澄清

`impl-weak-traits` 不是“带默认实现的正式后端库”，而是 `crate_interface` `weak_default` 测试矩阵中专门制造强覆盖场景的实现端测试资产；它存在的意义是让链接器优先级可以被稳定复现和断言。

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
| 实现侧 | `impl-weak-traits` | 负责导出强覆盖样例 |
| 消费侧 | `test-weak` | 负责链接并断言强覆盖行为 |
| 脚本入口 | `run_tests.sh weak` | nightly 路径的一键执行入口 |
| CI 入口 | `cargo run -p test-weak` | `crate_interface` CI 的弱默认多 crate 回归 |

## 4. 开发指南

### 4.1 什么时候应该修改它

只有在你想扩展 `weak_default` 的“强覆盖”覆盖面时，才应该改这个 crate，例如：

- 新增一个默认方法，需要验证显式覆写后是否真的压过默认实现。
- 新增一个同接口内部强弱混用的场景。
- 新增一个默认实现里含有 `Self::` 直接调用或间接引用的场景。

### 4.2 修改时的约束

- 新增或改动覆写逻辑后，要同步更新 `test-weak` 的断言。
- 不要把 `impl-weak-traits` 与 `impl-weak-partial` 放进同一最终二进制。
- 命名空间必须与定义侧保持完全一致。
- 覆写后的返回值最好与默认值拉开明显差距，便于一眼分辨命中路径。
- 公开实现类型不要轻易去掉，因为测试侧需要显式引用它们作为链接锚点。

### 4.3 不应该在这里做的事

- 不要把它当成可复用运行时后端。
- 不要为了“补齐 trait”而把所有默认方法都机械实现一遍，除非该场景本身就是测试目标。
- 不要在这里测试“未覆写时的纯回退”，那属于 `impl-weak-partial`。

## 5. 测试策略

### 5.1 当前测试方式

这个 crate 自己没有独立的 `tests/`，它的真实验证来自以下 nightly 多 crate 路径：

- `cargo +nightly run -p test-weak`
- `./run_tests.sh weak`
- `crate_interface` CI 中的 `Multi-crate test (with weak_default, nightly only)`

### 5.2 它在整体测试体系中的角色

`components/crate_interface/tests/test_weak_default.rs` 能验证最小化的“默认方法留空后仍可调用”语义，但它不足以覆盖完整的强覆盖路径。测试文件本身也说明，弱符号设计面向跨 crate 使用；而 README 更明确指出，带默认实现的接口不适合同 crate 做完整实现覆盖。

因此：

- 单 crate 单元测试负责验证 `weak_default` 的最小语义。
- `impl-weak-traits` 配合 `test-weak` 负责验证真正的跨 crate 强覆盖语义。

### 5.3 关注的风险点

- 若把另一份实现 crate 也链接进来，结果很容易从“断言失败”变成“重复符号”或“观测被污染”。
- 若覆盖值与默认值太接近，测试失败时难以判断到底走了哪条路径。
- 若只测 `Self::foo()` 的直接调用而不测函数引用路径，容易漏掉代理重写问题。

## 6. 跨项目定位

| 项目 | 与本 crate 的关系 | 实际意义 |
| --- | --- | --- |
| ArceOS | `ax-log`、`ax-runtime`、`ax-task`、`ax-driver` 等模块使用 `crate_interface`，但不会链接本 crate | 本 crate 间接保护 `crate_interface` 的实现端与链接期优先级语义 |
| StarryOS | 当前源码下未见 `os/StarryOS` 直接引用 `crate_interface` | 本 crate 更像仓库级能力保底，而不是 StarryOS 运行时依赖 |
| Axvisor | `axvisor_api` 通过宏封装 `crate_interface`，但不会直接使用本 crate | 本 crate 为 Axvisor 侧的底层接口桥接语义提供回归样本 |

## 7. 总结

`impl-weak-traits` 的价值，不在于它“实现了弱默认接口”，而在于它把 `crate_interface` 最难验证的一半语义钉成了可重复实验：强符号真的会压过弱默认，而且默认实现内部的 `Self::` 代理也会一起跟着跳转。它是实现端测试资产，不是运行时组件。
