# `ax-ctor-bare-macros` 技术文档

> 路径：`components/ctor_bare/ctor_bare_macros`
> 类型：过程宏库
> 分层：组件层 / 构造函数注册编译期辅助
> 版本：`0.2.1`
> 文档依据：当前仓库源码、`components/ctor_bare/Cargo.toml`、`components/ctor_bare/README.md`、`components/ctor_bare/ctor_bare_macros/Cargo.toml`、`components/ctor_bare/ctor_bare_macros/src/lib.rs`、`components/ctor_bare/ctor_bare/tests/*`

`ax-ctor-bare-macros` 是 `ax-ctor-bare` 的编译期搭档。它的职责只有一个：把一个普通 Rust 函数改写成“可被放进 `.init_array` 的构造函数入口”。它不是初始化运行时，也不负责遍历 `.init_array`，更不是完整的启动编排框架。

## 1. 架构设计分析

### 1.1 设计定位

这个 crate 提供的公开接口只有一个属性宏：

- `#[register_ctor]`

因此它的定位非常纯粹：

- 在编译期检查函数签名是否合法
- 生成放入 `.init_array` 的静态函数指针
- 生成最终会被调用的 `extern "C"` 函数实体

它只处理“登记”，不处理“执行”。

### 1.2 宏展开的真实内容

当 `#[register_ctor]` 应用到一个函数上时，宏会做三类事情：

1. 校验属性参数必须为空
2. 校验目标 item 必须是函数，且：
   - 没有输入参数
   - 没有返回值
3. 生成两段代码：
   - 一个放进 `.init_array` 的静态函数指针
   - 一个 `pub extern "C" fn` 的真实函数定义

生成出来的静态项形如：

- `_INIT_<函数名>`

并带有：

- `#[unsafe(link_section = ".init_array")]`
- `#[used]`

这表明它的本质是“把函数地址写进一个约定段”。

### 1.3 为什么要重新生成函数

宏不会直接把原始函数原封不动塞进 `.init_array`，而是会生成：

- `#[unsafe(no_mangle)] pub extern "C" fn name() { ... }`

这么做有几个现实目的：

- 保证段里放的是统一 ABI 的函数指针
- 让符号名稳定
- 让构造函数调用约定尽可能简单

因此 `ax-ctor-bare-macros` 实际上还承担了一个“小型 ABI 规整器”的角色。

### 1.4 错误诊断策略

源码中可以看到，以下输入都会在编译期被拒绝：

- 给 `#[register_ctor(...)]` 传参数
- 标注到非函数 item 上
- 函数有参数
- 函数有返回值

这些限制不是保守，而是为了保证 `ax_ctor_bare::call_ctors()` 能无条件把段里的项目当作 `fn()` 调用。

### 1.5 与 `ax-ctor-bare` 的真实关系

当前仓库中，测试代码和最终用户都通过：

- `ax_ctor_bare::register_ctor`

来使用这个宏，而不是直接依赖 `ax-ctor-bare-macros`。这说明它是一个典型的“内部辅助宏 crate”：

- `ax-ctor-bare-macros` 负责登记
- `ax-ctor-bare` 负责运行时遍历执行

## 2. 核心功能说明

### 2.1 主要能力

- 提供 `#[register_ctor]`
- 编译期校验构造函数签名
- 把函数指针写入 `.init_array`
- 生成统一 ABI 的构造函数入口
- 为错误输入生成清晰的 `compile_error`

### 2.2 当前仓库中的真实调用链

当前真实链路是：

1. 用户代码写 `#[register_ctor] fn foo() { ... }`
2. `ax-ctor-bare-macros` 生成 `.init_array` 静态项和 `extern "C"` 函数
3. `ax_ctor_bare::call_ctors()` 运行时遍历并执行这些函数

因此这个 crate 只参与第 1 到第 2 步，且完全属于编译期。

### 2.3 最关键的边界澄清

`ax-ctor-bare-macros` 不负责：

- 何时调用构造函数
- 调用多少次
- 构造函数之间的顺序与依赖
- 链接脚本中 `.init_array` 的布局

这些都属于 `ax-ctor-bare` 或更上层运行时/链接配置的职责。

## 3. 依赖关系图谱

### 3.1 直接依赖

| 依赖 | 作用 |
| --- | --- |
| `proc-macro2` | token 处理 |
| `quote` | 生成展开代码 |
| `syn` | 解析输入函数 |

### 3.2 主要消费者

直接消费者：

- `ax-ctor-bare`

通过 `ax-ctor-bare` 间接接入的链路：

- `ax-ctor-bare-macros` -> `ax-ctor-bare` -> `ax-runtime` -> 上层系统启动路径

### 3.3 关系解读

| 层级 | 角色 |
| --- | --- |
| `ax-ctor-bare-macros` | 编译期注册器 |
| `ax-ctor-bare` | 运行时执行器 |
| `ax-runtime` | 决定调用时机的系统启动层 |

## 4. 开发指南

### 4.1 什么时候应该改这个 crate

只有在以下情况才需要直接改动：

- 想改变 `#[register_ctor]` 的输入约束
- 想改变生成的符号/段布局
- 想优化编译期错误信息

如果只是调整构造函数的执行时机或调用方式，应去改 `ax-ctor-bare` 或上层运行时。

### 4.2 维护时要特别小心的点

- `.init_array` 段名不能轻易改
- 生成函数的 ABI 和可见性要与运行时预期匹配
- `_INIT_<name>` 的命名冲突风险要考虑
- 错误消息应保持可读，因为这是用户最直接接触的反馈面

### 4.3 推荐补强方向

- `trybuild` 类 compile-fail 测试
- 对非函数 item 的更精确诊断
- 对多属性叠加时的展开行为测试

但仍应保持它只是一个“小而专”的登记宏。

## 5. 测试策略

### 5.1 当前覆盖情况

该 crate 自身没有独立测试目录，但当前实际已被以下测试间接覆盖：

- `components/ctor_bare/ctor_bare/tests/test_ctor.rs`
- `components/ctor_bare/ctor_bare/tests/test_empty.rs`

因为这些测试通过 `ax_ctor_bare::register_ctor` 实际触发了本宏的展开。

### 5.2 建议补充的测试

- compile-fail：带参数、非函数、带返回值、带输入参数
- compile-pass：多个构造函数同时存在
- 展开稳定性测试：确保 `.init_array` 和 `extern "C"` 约定不被意外改坏

### 5.3 风险点

- 这是段布局契约的一部分，任何符号或 ABI 变化都会影响运行时
- 若文档不强调“不要直接用该 crate”，调用者可能绕过 `ax-ctor-bare` 门面
- 如果误把它当成运行时库看待，会把职责写错

## 6. 跨项目定位分析

| 项目 | 位置 | 角色 | 说明 |
| --- | --- | --- | --- |
| ArceOS | 共享启动基础设施 | 构造函数登记宏 | 通过 `ax-ctor-bare` 和 `ax-runtime` 间接进入启动链 |
| StarryOS | 共享启动基础设施 | 构造函数登记宏 | 若复用同一运行时路径则间接受益 |
| Axvisor | 共享启动基础设施 | 构造函数登记宏 | 通过共享运行时组件间接使用 |

## 7. 总结

`ax-ctor-bare-macros` 是一个非常纯粹的过程宏辅助 crate。它做的事情只有“把函数登记到 `.init_array` 并规整成可调用入口”，既不执行这些函数，也不管理启动流程。最重要的边界是：它是**注册器**，不是运行时。
