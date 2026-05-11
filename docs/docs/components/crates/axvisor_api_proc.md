# `axvisor_api_proc` 技术文档

> 路径：`components/axvisor_api/axvisor_api_proc`
> 类型：过程宏库
> 分层：组件层 / 编译期 API 生成辅助
> 版本：`0.1.0`
> 文档依据：当前仓库源码、`Cargo.toml`、`README.md`、`src/lib.rs`、`src/items.rs`、`components/axvisor_api/src/lib.rs`、`components/axvisor_api/src/test.rs`

`axvisor_api_proc` 不是运行时 API 库，而是 `axvisor_api` 背后的**过程宏辅助 crate**。它的职责是把一组带 `extern fn` 语法标记的模块，展开成可调用的 API 包装函数和对应的 `crate_interface` trait/impl 胶水代码。因此它属于“编译期 glue”，而不是 Hypervisor 子系统本体。

## 1. 架构设计分析

### 1.1 设计定位

这个 crate 解决的是 Axvisor API 组织方式问题：

- 如何让“定义 API 的模块”写起来像普通 Rust 模块
- 如何让“实现这些 API 的模块”在另一个位置编写
- 如何在编译期自动生成跨模块绑定代码

其公开入口只有两个属性宏：

- `#[api_mod]`
- `#[api_mod_impl(path)]`

这说明它的本质是一套**模块级 API DSL**，而不是运行时组件。

### 1.2 语法模型

`src/items.rs` 定义了宏要解析的核心语法节点：

| 类型 | 作用 |
| --- | --- |
| `ItemApiFn<T>` | 表示一个 API 函数；定义态用 `;`，实现态用 `Block` |
| `ApiModItem<T>` | 模块中的条目，区分普通 item 和 API 函数 |
| `ItemApiMod<T>` | 带有 API 语义的模块定义 |
| `ItemApiModDef` | `#[api_mod]` 使用的定义态模块 |
| `ItemApiModImpl` | `#[api_mod_impl]` 使用的实现态模块 |

这里最关键的设计是：

- API 函数用 `extern fn ...;` 或 `extern fn ... { ... }` 作为标记
- 普通 item 则原样保留

这样宏就能在一个模块里同时容纳：

- 类型别名
- `use` / `pub use`
- 普通辅助函数
- 真正需要跨组件绑定的 API 函数

### 1.3 `api_mod` 的展开思路

`#[api_mod]` 处理的是“定义 API”的模块。其关键步骤是：

1. 解析输入模块
2. 把模块内容分成普通条目和 API 函数
3. 生成一个隐藏 trait，名字形如 `Axvisor{Module}ApiTrait`
4. 给每个 API 函数生成一个同名包装函数
5. 包装函数内部用 `crate_interface::call_interface!` 转调

这些隐藏 trait 不是直接引用 `crate_interface` 路径，而是通过：

- `axvisor_api::__priv::crate_interface`

来拿到 `def_interface` / `call_interface`，从而避免调用方必须手工了解底层依赖布局。

### 1.4 `api_mod_impl` 的展开思路

`#[api_mod_impl(path)]` 处理的是“实现 API”的模块。其关键步骤是：

1. 解析被实现模块的路径
2. 复用该路径并生成一个隐藏别名，避免 trait 路径找错
3. 收集实现态模块里的 `extern fn`
4. 生成一个隐藏结构体 `__Impl`
5. 用 `crate_interface::impl_interface` 为对应 trait 生成实现

这说明 `axvisor_api_proc` 本质上不是在“注册 handler”，而是在**替调用方生成 trait + 调用桥 + 实现桥**。

### 1.5 关键约束

源码已经把这套 DSL 的边界写得很清楚：

- `api_mod` 不接受任何参数
- `api_mod` 只支持内联模块，不支持 `mod foo;` 这种 outlined module
- API 函数不能带 `self` 接收者
- `axvisor_api` crate 必须能被找到，否则直接生成 `compile_error!`

这些限制都不是偶然的，而是为了让宏展开后的 trait 绑定保持简单和稳定。

### 1.6 与 `axvisor_api` 的真实关系

`components/axvisor_api/src/lib.rs` 直接：

- `pub use axvisor_api_proc::{api_mod, api_mod_impl};`

并用这两个宏定义了 `memory`、`time`、`vmm`、`host`、`arch` 等 API 模块。随后：

- `arm_vcpu`
- `arm_vgic`
- `axdevice`
- `axvcpu`
- `axvm`
- `riscv_vcpu`
- `riscv_vplic`
- `x86_vcpu`
- `os/axvisor`

等 Axvisor 相关组件通过 `axvisor_api` 间接依赖这套宏展开结果。

因此，`axvisor_api_proc` 的真实地位是：**Axvisor API 组织方式的编译期基础设施**。

## 2. 核心功能说明

### 2.1 主要能力

- 解析带 `extern fn` 标记的 API 模块
- 为 API 模块生成隐藏 trait
- 为 API 定义侧生成包装调用函数
- 为 API 实现侧生成 `__Impl` 和 trait 实现
- 通过 `proc_macro_crate` 自动定位 `axvisor_api` crate 路径

### 2.2 真实调用链

真实的使用链路是：

1. `axvisor_api` 用 `#[api_mod]` 定义 API 模块
2. 宏展开后得到普通函数 + 隐藏 trait
3. 某个实现模块用 `#[api_mod_impl(...)]` 生成实现
4. 调用方像调用普通函数一样调用 `axvisor_api::memory::alloc_frame()` 等接口
5. 底层再通过 `crate_interface` 完成绑定

这条链路里，`axvisor_api_proc` 只参与第 1 到第 3 步，且全部发生在编译期。

### 2.3 最关键的边界

`axvisor_api_proc` 不提供：

- 运行时 API 注册表
- 动态查找或插件机制
- 真正的内存/时间/中断实现
- Hypervisor 状态管理

它只负责**把一套模块语法展开成编译期胶水代码**。

## 3. 依赖关系图谱

### 3.1 直接依赖

| 依赖 | 作用 |
| --- | --- |
| `syn` | 解析模块和函数语法树 |
| `quote` | 生成展开代码 |
| `proc-macro2` | token 处理 |
| `proc-macro-crate` | 定位 `axvisor_api` 在当前依赖图中的真实路径 |

### 3.2 主要消费者

直接消费者：

- `axvisor_api`

通过 `axvisor_api` 间接接入的主要组件：

- `arm_vcpu`
- `arm_vgic`
- `axdevice`
- `axvcpu`
- `axvm`
- `riscv_vcpu`
- `riscv_vplic`
- `x86_vcpu`
- `os/axvisor`

### 3.3 关系解读

| 层级 | 角色 |
| --- | --- |
| `axvisor_api_proc` | 编译期属性宏生成器 |
| `axvisor_api` | 对外 API 门面 crate |
| 各虚拟化组件 | 通过 `axvisor_api` 使用展开后的接口 |

## 4. 开发指南

### 4.1 什么时候应该改这个 crate

只有在以下情况才应直接修改：

- `api_mod` / `api_mod_impl` 的语法要扩展
- 展开后的 trait 或包装函数要调整
- `axvisor_api` crate 名解析策略要修正
- 需要改进错误诊断或文档生成

如果只是新增一个 Axvisor API，通常应该改 `axvisor_api/src/lib.rs` 或对应实现模块，而不是改宏 crate 本身。

### 4.2 维护时最容易出错的点

- `items.rs` 的语法定义与 `lib.rs` 的展开逻辑必须同步
- `api_mod` 和 `api_mod_impl` 使用的 trait 命名规则必须一致
- 被实现模块路径的复用标识不能丢，否则生成的 `impl` 可能找不到 trait
- `extern fn` 是 DSL 标记，不是普通 FFI 语义，相关文档必须讲清楚

### 4.3 扩展方向建议

若未来要增强此 crate，优先考虑：

- compile-pass / compile-fail 友好的错误信息
- 对不支持语法的明确诊断
- 更稳定的展开快照测试

而不应把任何运行时状态塞进宏展开结果里。

## 5. 测试策略

### 5.1 当前覆盖情况

该 crate 自身没有单独的 `tests/`。当前主要依赖：

- `axvisor_api/src/test.rs`
- `components/axvisor_api/examples/example.rs`

来间接验证宏的可用性。

### 5.2 建议补充的测试

- compile-pass：合法 `api_mod` / `api_mod_impl` 组合
- compile-fail：`api_mod` 传参、outlined module、`self` 接收者等非法输入
- 展开快照：验证隐藏 trait 和包装函数名称是否稳定
- crate 重命名场景：验证 `proc_macro_crate` 的路径发现逻辑

### 5.3 风险点

- 过程宏问题往往在编译期暴露，出错体验直接影响所有 API 用户
- 一旦 trait 命名规则变化，定义侧和实现侧都会一起失配
- 若误把宏 crate 当作运行时库理解，文档和调用方式都容易写偏

## 6. 跨项目定位分析

| 项目 | 位置 | 角色 | 说明 |
| --- | --- | --- | --- |
| ArceOS | 当前仓库未见直接接入 | 编译期辅助组件 | 不属于 ArceOS 主线 |
| StarryOS | 当前仓库未见直接接入 | 编译期辅助组件 | 尚未看到直接消费者 |
| Axvisor | `axvisor_api` 背后的宏基础设施 | API 定义/实现生成器 | 为 Axvisor 各组件提供统一 API 模块写法 |

## 7. 总结

`axvisor_api_proc` 是一个典型的“过程宏辅助层”。它最重要的价值，不是自己暴露了多少 API，而是把 Axvisor 的 API 定义与实现组织成统一的模块化写法，并在编译期自动生成 `crate_interface` 胶水。理解它时要牢牢记住：它是**宏工具**，不是运行时子系统。
