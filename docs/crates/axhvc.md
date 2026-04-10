# `axhvc` 技术文档

> 路径：`components/axhvc`
> 类型：库 crate
> 分层：组件层 / HyperCall ABI 定义组件
> 版本：`0.2.0`
> 文档依据：当前仓库源码、`Cargo.toml`、`README.md`、`src/lib.rs`、`os/axvisor/src/vmm/hvc.rs`

`axhvc` 的真实定位是 **AxVisor HyperCall ABI 描述层**。它负责把“来宾发起的 hypercall 编号”定义为稳定的 Rust 类型，并给出统一的结果类型；它并不负责 trap 入口、寄存器编组、权限校验、实际功能实现，也不是完整的 hypercall 子系统。

## 1. 架构设计分析

### 1.1 设计定位

整个 crate 实际上只有一个核心问题：

- 如何把来宾传入的 hypercall 编号稳定地转成类型安全的枚举

因此它的接口非常收敛，主要由三部分组成：

- `HyperCallCode`
- `InvalidHyperCallCode`
- `HyperCallResult`

这说明它是一个 ABI 契约 crate，而不是运行时组件。

### 1.2 `HyperCallCode` 的职责边界

`HyperCallCode` 使用 `#[repr(u32)]` 定义了当前支持或预留的 hypercall 编号：

| 编号 | 枚举 | 语义 |
| --- | --- | --- |
| `0` | `HypervisorDisable` | 请求关闭 hypervisor |
| `1` | `HyperVisorPrepareDisable` | 关闭前准备 |
| `2` | `HyperVisorDebug` | 调试用途 |
| `3` | `HIVCPublishChannel` | 发布 IVC 共享通道 |
| `4` | `HIVCSubscribChannel` | 订阅 IVC 共享通道 |
| `5` | `HIVCUnPublishChannel` | 取消发布 IVC 通道 |
| `6` | `HIVCUnSubscribChannel` | 取消订阅 IVC 通道 |

这些定义表达的是“编号到语义名称”的关系，而不是“功能已完整实现”的保证。

### 1.3 类型转换与格式化

源码中实现了两个非常关键的辅助能力：

- `TryFrom<u32> for HyperCallCode`
- 自定义 `Debug for HyperCallCode`

其中：

- `TryFrom<u32>` 用于把来宾传来的原始编号做类型安全转换
- `InvalidHyperCallCode` 保留非法数值，便于错误报告
- `Debug` 会同时打印名称和十六进制值，适合日志输出

这套设计说明 `axhvc` 主要面向：

- trap 分发前的编号解析
- hypercall 调试日志
- ABI 文档与来宾侧调用约定的共享

### 1.4 `HyperCallResult`

`HyperCallResult` 只是：

- `type HyperCallResult = AxResult<usize>`

也就是说，它统一了“hypercall 成功返回一个 `usize`，失败返回 `ax-errno` 错误”的 ABI 表达方式，但并没有引入更高层的返回值协议。

### 1.5 与 Axvisor 当前实现的真实关系

当前仓库里的真实消费者是 `os/axvisor/src/vmm/hvc.rs`。那里会：

1. 从 trap 参数里拿到原始 hypercall 编号
2. 用 `HyperCallCode::try_from(code as u32)` 做检查
3. 在 `execute()` 中按枚举分发

但需要特别注意的是：当前 Axvisor 只实现了四个 IVC 相关操作：

- `HIVCPublishChannel`
- `HIVCUnPublishChannel`
- `HIVCSubscribChannel`
- `HIVCUnSubscribChannel`

而 `HypervisorDisable`、`HyperVisorPrepareDisable`、`HyperVisorDebug` 虽然已在 `axhvc` 中定义，但在当前 `execute()` 实现里并没有单独分支，最终会落入：

- `_ => Unsupported`

这正好说明 `axhvc` 定义的是 ABI 空间，而不是“当前已实现功能全集”。

## 2. 核心功能说明

### 2.1 主要能力

- 定义 HyperCall 编号枚举
- 对原始 `u32` 编号做安全转换
- 为非法编号提供错误类型
- 统一 hypercall 返回值类型
- 为日志和调试提供可读输出

### 2.2 典型调用链

当前真实调用链可概括为：

1. 来宾触发 hypercall trap
2. Axvisor trap 处理逻辑收集编号和参数
3. `axhvc::HyperCallCode::try_from()` 把编号转成枚举
4. Axvisor 按 `HyperCallCode` 分发到 IVC 发布/订阅等逻辑

因此 `axhvc` 处于“trap 参数”与“Hypervisor 业务实现”之间的 ABI 边界层。

### 2.3 最关键的边界澄清

`axhvc` 不负责：

- 保存 hypercall 参数
- 规定不同架构上的寄存器传参细节
- 执行任何 hypercall 行为
- 分配共享内存或注入中断

它只负责定义**代码点和返回类型**。

## 3. 依赖关系图谱

### 3.1 直接依赖

| 依赖 | 作用 |
| --- | --- |
| `ax-errno` | 统一 HyperCall 错误返回类型 |

### 3.2 主要消费者

当前仓库内可确认的直接消费者是：

- `os/axvisor`

实际调用链为：

- `axhvc` -> `os/axvisor/src/vmm/hvc.rs`

### 3.3 关系解读

| 关系 | 说明 |
| --- | --- |
| `axhvc` -> `os/axvisor` | 提供 hypercall 编号与结果类型 |
| `os/axvisor` -> `axvm` / `axdevice` | 在具体分支里再调用 VM、IVC、映射等逻辑 |

换句话说，`axhvc` 在依赖图上很靠近入口，但它本身并不承载下游逻辑。

## 4. 开发指南

### 4.1 什么时候应该改这个 crate

只有在以下情况才应该改 `axhvc`：

- 新增一个正式的 hypercall 编号
- 废弃或保留某个编号
- 需要改变编号与名字的映射关系
- 统一 hypercall 返回值语义

如果只是修改某个 hypercall 的实现逻辑，应优先改 Axvisor 的分发器和具体实现，而不是改这里。

### 4.2 新增 HyperCall 的同步修改点

新增编号时，至少要同步检查：

1. `HyperCallCode` 枚举
2. `TryFrom<u32>` 匹配分支
3. `Debug` 输出
4. `README` 和调用方文档
5. Axvisor 侧的分发实现

否则会出现“ABI 已声明，但分发器不认识”或“编号已存在，但调试输出不完整”的不一致。

### 4.3 ABI 维护注意事项

- 编号一旦对外公开，就应视为 ABI，不能随意重排
- 来宾与 Hypervisor 必须共享同一版本的编号定义
- 若某编号只是预留而未实现，文档中必须明确说明

## 5. 测试策略

### 5.1 当前覆盖情况

crate 目录里没有单独测试。当前主要依赖：

- 编译检查
- Axvisor 的运行时分发路径

### 5.2 建议补充的单元测试

- 验证每个合法编号都能正确 `try_from`
- 验证非法编号会返回 `InvalidHyperCallCode`
- 验证 `Debug` 输出格式稳定
- 验证 `HyperCallResult` 在典型错误路径中的可用性

### 5.3 建议补充的集成测试

- 来宾发起已实现 IVC hypercall 的端到端测试
- 对未实现但已定义的编号验证是否稳定返回 `Unsupported`
- 对非法编号验证 Axvisor 是否能稳定拒绝

### 5.4 风险点

- ABI 定义与 Axvisor 实现不同步会直接导致来宾/Hypervisor 协议错位
- 新增编号但忘记更新 `TryFrom` 会造成无法识别
- 文档里把“已定义”误写成“已实现”会误导来宾侧接入

## 6. 跨项目定位分析

| 项目 | 位置 | 角色 | 说明 |
| --- | --- | --- | --- |
| ArceOS | 当前仓库未见直接接线 | 共享 ABI 组件 | 本身不属于 ArceOS 运行时主线 |
| StarryOS | 当前仓库未见直接接线 | 共享 ABI 组件 | 尚未看到直接集成 |
| Axvisor | HyperCall 入口边界 | HyperCall 编号与返回值定义层 | 被 `os/axvisor/src/vmm/hvc.rs` 直接使用 |

## 7. 总结

`axhvc` 的重要性不在于“代码多”，而在于它定义了来宾和 Axvisor 之间的 hypercall 编号契约。理解它时最容易犯的错误，就是把它看成 hypercall 子系统本身。实际上，它只是**ABI 定义层**：负责把编号和类型讲清楚，把执行留给真正的 Hypervisor 逻辑。
