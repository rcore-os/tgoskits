# `ax-cap-access` 技术文档

> 路径：`components/cap_access`
> 类型：库 crate
> 分层：组件层 / 能力位访问封装
> 版本：`0.3.0`
> 文档依据：当前仓库源码、`Cargo.toml`、`README.md`、`src/lib.rs`、`os/arceos/modules/axfs/src/fops.rs`

`ax-cap-access` 的真实定位是一个**能力位 bitmask + 包装器**组件：它把“某个对象当前允许的访问权限”附着在对象句柄上，并在访问时做一次显式的 capability 子集检查。它不是完整的 capability 安全子系统，不做全局对象管理、权限传播、撤销、审计或命名空间隔离。

## 1. 架构设计分析

### 1.1 设计定位

这个 crate 的接口极小，核心就两个类型：

- `Cap`
- `WithCap<T>`

其中：

- `Cap` 是能力位集合
- `WithCap<T>` 是“对象 + 能力位”的组合包装

这说明它解决的不是“系统范围权限模型”，而是“**单个对象句柄的本地访问约束**”。

### 1.2 `Cap`：能力位集合

`Cap` 由 `bitflags` 生成，目前只有三个位：

- `READ`
- `WRITE`
- `EXECUTE`

这三个权限位对应的不是某种复杂策略语言，而是最基础的访问能力表达。它们的使用语义是：

- 请求的权限必须是已授予权限的子集

具体体现在 `can_access()` 内部使用的是：

- `self.cap.contains(requested_cap)`

### 1.3 `WithCap<T>`：句柄包装模型

`WithCap<T>` 只保存两项数据：

- `inner: T`
- `cap: Cap`

对外提供的能力也很克制：

- `new(inner, cap)`
- `cap()`
- `can_access(cap)`
- `access_unchecked()`
- `access(cap)`
- `access_or_err(cap, err)`

值得注意的是，它只返回：

- `&T`

而不是 `&mut T`。这意味着它本身不承担“可变借用权限模型”的表达。若调用方需要修改对象，通常依赖的是：

- 被包装对象本身的内部可变性
- 或其底层句柄语义

### 1.4 `unsafe` 边界

`access_unchecked()` 是该 crate 唯一显式越过能力检查的入口：

- 调用者需自行保证不违反能力约束

当前仓库里的真实使用场景是在 `ax-fs` 的 `Drop` 实现中：

- `File` / `Directory` 在析构时通过 `access_unchecked()` 取出 `VfsNodeRef`
- 然后调用 `release()`

这类用法说明：`ax-cap-access` 的能力检查主要针对“正常业务访问”，而某些生命周期收尾路径需要由上层自己背书。

### 1.5 与 `ax-fs` 的真实关系

当前仓库内可确认的直接消费者是：

- `os/arceos/modules/axfs/src/fops.rs`

在那里：

- `File` 和 `Directory` 都把 `VfsNodeRef` 包装成 `WithCap<VfsNodeRef>`
- `OpenOptions` 会被转换成 `Cap`
- 文件权限 `FilePerm` 也会被映射成 `Cap`
- 读、写、执行目录遍历分别通过 `Cap::READ`、`Cap::WRITE`、`Cap::EXECUTE` 做检查

例如：

- `read()` / `read_at()` 要求 `Cap::READ`
- `write()` / `truncate()` / `flush()` 要求 `Cap::WRITE`
- 相对目录访问 `access_at()` 要求 `Cap::EXECUTE`
- `get_attr()` 甚至允许用 `Cap::empty()` 访问元数据

这说明 `ax-cap-access` 在当前系统里承担的是**文件句柄级权限防线**，而不是 VFS 全局权限系统。

## 2. 核心功能说明

### 2.1 主要能力

- 用位标志表达对象可访问权限
- 在对象句柄外层附着权限集合
- 在访问时做显式能力检查
- 支持无检查访问，用于特殊受控路径
- 支持 `Option` 和 `Result` 风格的访问返回

### 2.2 当前仓库中的典型调用链

真实调用链可概括为：

1. `ax-fs` 根据 `OpenOptions` 和节点权限生成 `Cap`
2. 用 `WithCap::new(node, access_cap)` 封装已打开的节点
3. 各操作通过 `access_or_err()` 检查访问权限
4. 失败时统一映射为 `PermissionDenied`

因此 `ax-cap-access` 处于“文件对象已获得”之后、“具体操作开始”之前这一层。

### 2.3 最关键的边界澄清

`ax-cap-access` 不提供：

- 权限继承与传播
- 全局 capability 表
- capability 撤销
- 对象命名、查找和授权委托
- 安全审计日志

它就是一个**本地对象包装器**。

## 3. 依赖关系图谱

### 3.1 直接依赖

| 依赖 | 作用 |
| --- | --- |
| `bitflags` | 生成 `Cap` 能力位集合 |

### 3.2 主要消费者

当前仓库内可确认的直接消费者：

- `ax-fs`

当前最清晰的传递关系：

- `ax-cap-access` -> `ax-fs` -> 文件系统相关上层模块

### 3.3 关系解读

| 层级 | 角色 |
| --- | --- |
| `ax-cap-access` | 句柄级能力检查封装 |
| `ax-fs` | 把它用于文件/目录对象的访问限制 |
| 上层应用或系统调用层 | 通过 `ax-fs` 间接获得权限约束 |

## 4. 开发指南

### 4.1 适合什么时候使用

适合使用 `ax-cap-access` 的场景是：

- 已经拿到对象句柄
- 只想在对象访问前做一层轻量能力位检查
- 需要很低的依赖复杂度

如果需要的是：

- 全局授权模型
- capability 复制/转移/撤销
- 多主体安全策略

则应在更高层实现，不要强行扩展这个 crate。

### 4.2 维护时的关键注意事项

- `contains()` 代表“请求权限必须是授予权限的子集”，不要把方向写反
- 如果新增权限位，要同步更新消费者里的权限映射
- `access_unchecked()` 只能放在外部不变量已充分成立的路径
- 当前接口只暴露共享引用，若引入可变访问要重新审视边界

### 4.3 在 `ax-fs` 场景中的经验

- `Cap::empty()` 可用于无需读写执行权限的元数据访问
- 相对目录访问为什么要 `EXECUTE`，应在消费者文档中继续保留这一语义
- `WithCap` 只负责“访问前检查”，不负责打开/关闭对象生命周期

## 5. 测试策略

### 5.1 当前覆盖情况

crate 本身没有独立测试，当前主要依赖：

- README 中的示例语义
- `ax-fs` 的真实调用路径

### 5.2 建议补充的单元测试

- `READ` / `WRITE` / `EXECUTE` 及其组合的子集判断
- `Cap::empty()` 的语义
- `access()` 与 `access_or_err()` 的返回行为
- `access_unchecked()` 的说明性测试

### 5.3 建议补充的集成测试

- `ax-fs` 中文件只读、只写、目录遍历等典型路径
- `PermissionDenied` 的映射是否正确
- 析构阶段 `access_unchecked()` 是否始终只用于释放句柄

### 5.4 风险点

- 它过于轻量，最容易被误写成“完整 capability 系统”
- 若消费者权限映射有误，`ax-cap-access` 本身无法替你纠正策略
- `unsafe` 入口虽小，但若滥用会直接绕过整个模型

## 6. 跨项目定位分析

| 项目 | 位置 | 角色 | 说明 |
| --- | --- | --- | --- |
| ArceOS | `ax-fs` 文件对象访问层 | 句柄级能力封装 | 当前最明确的直接使用场景 |
| StarryOS | 共享文件系统基础设施 | 句柄级能力封装 | 若复用 `ax-fs` 路径则会间接带入 |
| Axvisor | 当前仓库未见直接接线 | 通用能力封装候选组件 | 尚未看到独立使用 |

## 7. 总结

`ax-cap-access` 的关键价值，不是“能力系统做得多复杂”，而是它把对象句柄权限检查压缩成了一个非常清晰的小抽象：`Cap + WithCap<T>`。理解它时一定要守住边界，它是**能力位包装器**，不是完整的安全子系统。
