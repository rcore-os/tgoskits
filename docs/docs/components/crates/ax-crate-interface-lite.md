# `ax-crate-interface-lite` 技术文档

> 路径：`components/crate_interface/crate_interface_lite`
> 类型：库 crate
> 分层：组件层 / 轻量级接口桥接组件
> 版本：`0.1.0`
> 文档依据：当前仓库源码、`Cargo.toml`、`README.md`、`src/lib.rs`、`tests/test_crate_interface.rs`、同仓库 `components/crate_interface/*`

`ax-crate-interface-lite` 的定位很明确：它是 `crate_interface` 的一个**轻量替代实现**，用声明宏而不是过程宏来建立“在一个 crate 定义 trait、在另一个 crate 提供实现、再在任意地方静态调用”的桥接关系。它不是服务发现框架，不是运行时插件系统，也不是依赖注入容器。

## 1. 架构设计分析

### 1.1 设计定位

这个 crate 试图解决的问题是：

- 跨 crate 共享一组静态接口
- 避免引入 proc-macro 依赖
- 保持调用体验尽量接近 `crate_interface`

因此它的核心公开接口并不是普通函数，而是三组宏：

- `def_interface!`
- `impl_interface!`
- `call_interface!`

以及一个仅供宏内部使用的私有模块：

- `r#priv::{DefaultImpl, MustNotAnAlias}`

### 1.2 `def_interface!` 的实现方式

`def_interface!` 并不是简单转发 trait 定义。它会额外生成两类东西：

1. 原始 trait 本体
2. 一个针对 `DefaultImpl` 的默认实现

这份默认实现里，每个 trait 方法都会被展开成：

- 一个 `extern "Rust"` 声明
- 带 `#[link_name = "__Trait__method"]` 的符号绑定

也就是说，`def_interface!` 的真实作用是：

- 在“定义侧”预先约定好符号名
- 让后续任意实现方只要导出同名符号即可被静态调用

### 1.3 `impl_interface!` 的实现方式

`impl_interface!` 做的事情与 `def_interface!` 正好相对：

- 为目标类型生成 trait 实现
- 在每个方法体里生成一个 `extern "Rust"` 函数
- 通过 `#[export_name = "__Trait__method"]` 把实现导出为约定符号

因此这套机制的本质不是 vtable，也不是动态 dispatch，而是：

- **基于链接符号名的静态接口桥接**

这也是它能摆脱 proc-macro 依赖、保持 `no_std` 友好的关键。

### 1.4 `call_interface!` 的实现方式

`call_interface!` 的目标是把：

- `Trait::method(...)`

这样的调用形式，最终转成：

- `<DefaultImpl as Trait>::method(...)`

它内部借助隐藏宏 `__interface_fn!` 逐段解析路径，因此支持：

- 相对路径
- 绝对路径
- 模块内、模块外调用

测试文件 `tests/test_crate_interface.rs` 就覆盖了：

- 当前模块调用
- 子模块调用
- 显式 `crate::` / `super::` 路径调用

### 1.5 `MustNotAnAlias`：反 trait alias 约束

`impl_interface!` 里有一个容易被忽略但很关键的设计：

- 在 trait impl 中插入 `const TraitName: MustNotAnAlias = MustNotAnAlias;`

这会强制调用者使用原始 trait 名，而不是 alias 名。README 和源码文档都明确指出：

- trait alias 不被支持

这是为了避免符号名推导与真实定义脱节。

### 1.6 与完整版 `crate_interface` 的关系

从当前仓库的真实调用关系看：

- `ax-crate-interface-lite` 主要在自己的测试里使用
- 真实系统组件更常使用完整的 `crate_interface`
- 例如 `axvisor_api` 通过 `crate_interface` 生成更复杂的 API 桥接

因此在本仓库里，`ax-crate-interface-lite` 更像一个“低依赖备用实现”或“最小语义验证版本”，而不是主流接口桥接底座。

## 2. 核心功能说明

### 2.1 主要能力

- 定义跨 crate 静态接口
- 为接口生成默认调用入口
- 在实现侧导出约定符号
- 在使用侧以宏形式调用接口
- 在不使用 proc-macro 的前提下完成跨 crate 绑定

### 2.2 当前实现的真实限制

README 与宏定义共同表明，它当前刻意保留了几个限制：

- 不支持属性宏写法，只支持声明宏写法
- 不支持方法接收者 `self` / `&self` / `&mut self`
- 不支持默认实现
- `impl_interface!` 语法要求 trait 名和目标类型名都是简单标识符

这些限制是“轻量化”的代价，也是它与完整 `crate_interface` 的主要边界。

### 2.3 最关键的边界澄清

`ax-crate-interface-lite` 不是：

- 运行时注册中心
- 可热插拔插件机制
- 依赖注入容器
- 动态多实现分派系统

它只是把“一个全局实现”通过静态链接符号桥接到调用点。

## 3. 依赖关系图谱

### 3.1 直接依赖

该 crate 没有额外依赖，纯靠 `core` 与 `macro_rules!` 工作。

### 3.2 主要消费者

当前仓库内可确认的真实消费者主要是：

- `components/crate_interface/crate_interface_lite/tests/test_crate_interface.rs`

与之对照的真实现象是：

- 主工作区其他组件目前主要使用完整版 `crate_interface`
- 尚未看到 ArceOS / StarryOS / Axvisor 主线直接依赖 `ax-crate-interface-lite`

### 3.3 关系解读

| 关系 | 说明 |
| --- | --- |
| `ax-crate-interface-lite` -> 自测用例 | 验证 define/impl/call 的最小闭环 |
| 完整版 `crate_interface` -> 系统主线组件 | 当前仓库里更常见的实际接线方式 |

## 4. 开发指南

### 4.1 适合什么时候使用

当你需要：

- 非常小的依赖面
- 不想引入 proc-macro
- 接口全是静态函数风格
- 单一实现就够用

可以优先考虑 `ax-crate-interface-lite`。

当你需要：

- 更灵活的宏形式
- 更复杂的接口定义方式
- 与现有主线组件保持一致

则更可能应使用完整版 `crate_interface`。

### 4.2 编写接口时的注意事项

- `def_interface!` 中的方法必须是普通静态函数签名
- 不要写 `self` 接收者
- 不要写默认实现
- 不要用 trait alias 去实现接口
- 定义、实现、调用三侧必须最终链接到同一个二进制里

### 4.3 维护宏时的注意事项

- `link_name` / `export_name` 的命名协议不能轻易改
- `DefaultImpl` 和 `MustNotAnAlias` 是接口契约的一部分
- 若扩展语法，要同步补测试覆盖跨模块和跨路径调用场景

## 5. 测试策略

### 5.1 当前覆盖情况

已有测试 `tests/test_crate_interface.rs` 覆盖了：

- 接口定义
- 接口实现
- 直接调用
- 模块内/模块外路径调用

这是当前最真实的调用关系来源。

### 5.2 建议补充的测试

- compile-fail：trait alias、`self` 接收者、默认实现
- 跨 crate 测试：把定义、实现、调用拆到三个 crate
- 符号冲突测试：多个接口共存时的命名稳定性

### 5.3 风险点

- 这套机制依赖符号命名协议，符号名改动会直接破坏兼容性
- 用户若把它当成多实现动态系统，会误解其使用边界
- 当前限制较多，文档不写清楚很容易被误用

## 6. 跨项目定位分析

| 项目 | 位置 | 角色 | 说明 |
| --- | --- | --- | --- |
| ArceOS | 当前仓库未见主线接入 | 轻量级接口桥接候选组件 | 目前未成为主线基础设施 |
| StarryOS | 当前仓库未见主线接入 | 轻量级接口桥接候选组件 | 尚未看到直接使用 |
| Axvisor | 当前仓库未见主线接入 | 轻量级接口桥接候选组件 | Axvisor 当前更依赖完整版 `crate_interface` 及其包装 |

## 7. 总结

`ax-crate-interface-lite` 的价值在于“够小、够直接、够静态”。它把跨 crate 接口桥接压缩成三组声明宏和一套链接符号协议，适合做低依赖的接口适配层。理解它时最重要的边界是：它是**静态链接层面的接口桥**，不是完整的接口管理框架。
