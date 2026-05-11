# `ax-ctor-bare` 技术文档

> 路径：`components/ctor_bare/ctor_bare`
> 类型：库 crate
> 分层：组件层 / 裸机构造函数注册运行时
> 版本：`0.2.1`
> 文档依据：当前仓库源码、`components/ctor_bare/Cargo.toml`、`components/ctor_bare/README.md`、`components/ctor_bare/ctor_bare/Cargo.toml`、`components/ctor_bare/ctor_bare/src/lib.rs`、`components/ctor_bare/ctor_bare/tests/*`、`os/arceos/modules/axruntime/src/lib.rs`

`ax-ctor-bare` 的定位不是“初始化框架”，而是一个很小的**构造函数注册与遍历运行时**：配合 `#[register_ctor]` 宏把函数指针放进 `.init_array`，再在需要时用 `call_ctors()` 统一遍历执行。它不做依赖排序，不做分阶段启动，更不理解系统里的模块拓扑。

## 1. 架构设计分析

### 1.1 设计定位

这个 crate 解决的是 `no_std` / 自研内核场景下的一个经典问题：

- 如何像 C/C++ 的 `constructor` 一样，在系统进入主逻辑前执行一批初始化函数

为此，它采用了最传统、也最兼容链接器语义的方案：

- 把函数指针放进 `.init_array`
- 通过 `__init_array_start` / `__init_array_end` 获取该段范围
- 逐项调用其中的函数指针

### 1.2 核心组成

`ax-ctor-bare` 本体非常小，只有三项关键内容：

| 项目 | 作用 |
| --- | --- |
| `pub use ax_ctor_bare_macros::register_ctor` | 向外重新导出注册宏 |
| `_SECTION_PLACE_HOLDER` | 放在 `.init_array` 的零长度占位，保证起止符号可生成 |
| `call_ctors()` | 遍历 `.init_array` 并调用其中的构造函数 |

其中 `_SECTION_PLACE_HOLDER` 很关键。它不是业务数据，而是为链接器和符号边界服务的“段存在性占位”。

### 1.3 `call_ctors()` 的真实行为

`call_ctors()` 的实现完全围绕段遍历展开：

1. 取 `__init_array_start` 与 `__init_array_end`
2. 按函数指针大小 `step_by`
3. 读取当前槽位里的函数指针
4. `transmute` 成 `fn()` 后调用

这说明：

- `ax-ctor-bare` 只知道“这里有一串 `fn()` 指针”
- 它不知道每个函数来自哪个模块
- 它也不会做去重、排序、依赖解析或错误隔离

这正是它的边界所在。

### 1.4 与宿主环境的关系

README 已经说明了两种使用模式：

- 在 Linux/macOS 等常规环境里，系统加载器本身就会处理 `.init_array`
- 在自研内核或裸机环境里，需要手工调用 `ax_ctor_bare::call_ctors()`

当前仓库中，ArceOS 的真实接线点就是：

- `os/arceos/modules/axruntime/src/lib.rs`

在主 CPU 初始化流程中，`ax-runtime` 完成驱动、文件系统、网络、显示、中断等基础初始化后，会显式执行：

- `ax_ctor_bare::call_ctors();`

然后才继续进入 `main()`。

### 1.5 链接契约

这个 crate 与链接脚本/链接参数高度相关。README 明确提出两点要求：

- 自定义链接脚本必须保留 `.init_array` 并导出 `__init_array_start` / `__init_array_end`
- 需要加 `-z nostart-stop-gc`，避免相关段符号被优化掉

因此 `ax-ctor-bare` 不是一个“纯 Rust 逻辑库”，而是一个带明显链接器契约的组件。

## 2. 核心功能说明

### 2.1 主要能力

- 重新导出 `#[register_ctor]`
- 在运行时遍历 `.init_array`
- 调用所有已注册的 `fn()`
- 在“没有任何构造函数”的情况下仍然保证段边界符号可用

### 2.2 当前仓库中的真实调用链

当前真实链路是：

1. 业务函数上使用 `#[register_ctor]`
2. 宏 crate 把函数指针放进 `.init_array`
3. `ax-runtime` 启动时调用 `ax_ctor_bare::call_ctors()`
4. 各注册函数被依次执行

这说明 `ax-ctor-bare` 处于：

- 编译期注册结果
- 启动期统一执行入口

之间的运行时薄层。

### 2.3 最关键的边界澄清

`ax-ctor-bare` 不是：

- 依赖感知的初始化编排器
- 多阶段启动管理器
- 模块生命周期框架
- 带错误恢复能力的初始化中心

它只是“**一段函数指针数组的遍历器**”。

## 3. 依赖关系图谱

### 3.1 直接依赖

| 依赖 | 作用 |
| --- | --- |
| `ax-ctor-bare-macros` | 提供 `#[register_ctor]` 属性宏 |

### 3.2 主要消费者

直接消费者：

- `os/arceos/modules/axruntime`

可确认的间接链路：

- `ax-ctor-bare` -> `ax-runtime` -> ArceOS/StarryOS/Axvisor 的共享运行时启动路径

### 3.3 关系解读

| 层级 | 角色 |
| --- | --- |
| `ax-ctor-bare-macros` | 编译期登记函数指针 |
| `ax-ctor-bare` | 运行时遍历 `.init_array` |
| `ax-runtime` | 在系统启动序列中选择何时执行这些函数 |

## 4. 开发指南

### 4.1 适合用来做什么

适合放进 `#[register_ctor]` 的逻辑通常是：

- 不依赖复杂运行时状态的轻量初始化
- 必须尽早注册的全局表项
- 可以接受“多次调用也不出严重问题”的初始化动作

不适合放进去的是：

- 需要严格顺序依赖的复杂初始化链
- 依赖某些子系统已完全就绪的逻辑
- 不可重入、不可重复执行的敏感代码

测试 `test_ctor.rs` 已明确说明构造函数既可能由宿主自动执行，也可能被手工再次调用。

### 4.2 维护时的关键注意事项

- 自定义链接脚本必须正确保留 `.init_array`
- 启动流程里要明确 `call_ctors()` 放在什么时机
- 若构造函数之间存在隐式顺序依赖，应在上层显式整理，而不是寄希望于链接顺序
- 构造函数签名必须保持 `fn()`，参数和返回值约束由宏 crate 保证

### 4.3 如果要扩展能力，应该放在哪一层

- 分阶段启动、依赖排序：放在上层运行时
- 注册函数的编译期约束：放在 `ax-ctor-bare-macros`
- 遍历 `.init_array` 的底层逻辑：保留在 `ax-ctor-bare`

## 5. 测试策略

### 5.1 当前覆盖情况

当前已有两个集成测试：

- `tests/test_ctor.rs`
- `tests/test_empty.rs`

它们分别覆盖：

- 宿主自动执行构造函数的常规路径
- 手工再次执行构造函数的可重入观察
- 没有任何构造函数时 `call_ctors()` 仍能安全运行

### 5.2 建议补充的测试

- 自定义链接脚本场景下的最小集成测试
- 构造函数顺序可观测测试
- 多次调用 `call_ctors()` 时的行为说明性测试
- 缺少 `.init_array` 或缺少链接参数时的构建文档/回归测试

### 5.3 风险点

- 链接脚本或链接参数错误会让整个机制失效
- 构造函数执行顺序并非该 crate 显式管理
- 把它误当作“初始化框架”会导致职责堆积和文档失真

## 6. 跨项目定位分析

| 项目 | 位置 | 角色 | 说明 |
| --- | --- | --- | --- |
| ArceOS | `ax-runtime` 启动路径 | 构造函数遍历运行时 | 当前最明确的真实接线点 |
| StarryOS | 共享运行时链路 | 启动期辅助组件 | 若复用同一 `ax-runtime` 路径则会间接使用 |
| Axvisor | 共享运行时链路 | 启动期辅助组件 | 通过共享运行时基础设施间接获得能力 |

## 7. 总结

`ax-ctor-bare` 的职责非常清楚：把 `.init_array` 里的函数指针按顺序调用出来。它是一个小型、低层、强链接器契约的运行时组件。理解它时必须避免一个常见误区：它不是完整的初始化子系统，只是**构造函数登记机制的运行时执行端**。
