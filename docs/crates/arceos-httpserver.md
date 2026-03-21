# `arceos-httpserver` 技术文档

> 路径：`os/arceos/examples/httpserver`
> 类型：二进制 crate
> 分层：ArceOS 层 / 系统行为样例
> 版本：`0.1.0`
> 文档依据：`Cargo.toml`、`src/main.rs`、`docs/arceos-internals.md`

`arceos-httpserver` 是一个极简的 ArceOS HTTP 服务端示例。它在 `0.0.0.0:5555` 上监听 TCP 连接，对每个连接读一次请求、回一次固定 HTML 页面，并通过 `thread::spawn` 为每个连接创建处理线程。这个 crate 的价值不在于提供 Web 服务器框架，而在于验证 `axstd` 的 `alloc`、`multitask`、`net` 三条系统能力是否已经能一起工作。

最关键的边界是：`arceos-httpserver` 只是系统行为样例，不是 ArceOS 的 HTTP 服务框架，更不是网络子系统本体。

## 1. 架构设计分析

### 1.1 设计定位

从 `Cargo.toml` 可以看出，它只声明了一个可选依赖：

```toml
axstd = { workspace = true, features = ["alloc", "multitask", "net"], optional = true }
```

这三个 feature 恰好对应它所需的三类能力：

- `alloc`：用于 `format!` 拼接响应字符串
- `multitask`：用于 `thread::spawn`
- `net`：用于 `TcpListener` / `TcpStream`

因此，这个示例本质上是一个“多任务 + 网络 + 分配器联合装配验证器”。

### 1.2 代码结构

源码只有几块核心内容：

| 组成 | 作用 |
| --- | --- |
| `LOCAL_IP` / `LOCAL_PORT` | 固定监听地址与端口 |
| `CONTENT` | 固定返回的 HTML 页面 |
| `header!` | 拼接最小 HTTP 响应头 |
| `http_server()` | 处理单个连接：读一次、回一次、关闭 |
| `accept_loop()` | 监听并为每个新连接创建线程 |
| `main()` | 打印提示并进入监听循环 |

这种组织方式没有业务抽象，目标就是用最少代码把系统能力串起来。

### 1.3 并发模型

`accept_loop()` 的行为非常直接：

1. 用 `TcpListener::bind((LOCAL_IP, LOCAL_PORT))` 建立监听 socket
2. 死循环 `accept()`
3. 每来一个连接就 `thread::spawn` 一个处理线程
4. 子线程调用 `http_server(stream)`，读取请求并写回固定页面

这意味着它验证的是“线程与 socket 是否能协作”，而不是高性能事件驱动 Web server 模型。

### 1.4 可观测性设计

源码里定义了一个极简 `info!` 宏，只在 `option_env!("LOG")` 为 `info`/`debug`/`trace` 时打印连接日志。这表明示例本身也在刻意保持最小依赖面，不额外引入日志框架。

## 2. 核心功能说明

### 2.1 实际提供的能力

- 启动一个固定端口的 TCP 监听器
- 为每个连接创建一个工作线程
- 读取一次请求
- 返回固定 `200 OK` HTML 页面
- 关闭连接

### 2.2 实际没有提供的能力

这个示例明确没有实现：

- 完整 HTTP 解析
- keep-alive
- chunked / streaming response
- 路由分发
- MIME / header 管理
- 限流、超时、错误恢复和生产级并发控制

因此，它更适合作为“网络 + 多任务组合 smoke test”，而不是 Web 服务基础库。

### 2.3 与系统装配链的关系

`docs/arceos-internals.md` 已把 `httpserver` 当作 feature 装配案例：应用只声明自己需要 `alloc`、`multitask`、`net`，而运行时、调度器、网络栈和驱动的实际装配由更下层负责。

换句话说，`arceos-httpserver` 的意义在于逼迫相关 feature 真正同时工作。

### 2.4 与性能工具的关系

源码开头已经给出 `ab -n 5000 -c 20` 的压力烟测示例，但这不代表它是性能基准框架。这里的 `ab` 用法更适合被理解为“验证每连接线程 + 监听路径在压力下仍能工作”的附加样例。

## 3. 依赖关系

### 3.1 直接依赖

| 依赖 | 作用 |
| --- | --- |
| `axstd`（可选） | 提供 `TcpListener`、`TcpStream`、`thread`、`format!` 等应用接口 |

### 3.2 间接依赖链

一旦启用 `axstd` 以及其 `alloc`、`multitask`、`net` 能力，该示例会间接依赖：

- `axruntime`
- `axtask`
- `axnet` 或 `axnet-ng`
- 更底层的驱动、内存与中断设施

这也是它比 `arceos-httpclient` 更容易暴露系统装配问题的原因之一。

### 3.3 跨层关系

| 层次 | 角色 |
| --- | --- |
| `arceos-httpserver` | 固定页面 + 最小监听/线程样例 |
| `axstd` | 向应用暴露 `std::net` 与线程接口 |
| `axruntime` | 把多任务与网络能力装入镜像 |
| `axnet*` | 提供真正的内核网络实现 |

## 4. 开发指南

### 4.1 运行方式

```bash
cargo xtask arceos run --package arceos-httpserver --arch riscv64 --net
```

运行后可用浏览器、`curl` 或 `ab` 访问 `http://<guest-ip>:5555/`。

### 4.2 修改时的建议

1. 如果目标只是验证网络与线程组合，请保持示例尽量简单。
2. 如果想测试更复杂的应用层协议行为，建议另建专门示例，不要让这个最小样例越长越重。
3. 如果改动涉及线程模型、监听端口、日志输出，应明确这是在调整“示例行为”，不是在修改网络基础设施。

### 4.3 高风险点

- `thread::spawn` 依赖 `multitask`，若 feature 装配有问题会直接暴露
- 只读一次请求、只写一次响应，容易掩盖分段收发问题
- 固定监听地址和端口更适合 smoke test，不适合复杂部署场景

## 5. 测试策略

### 5.1 当前测试形态

没有独立单元测试。这个示例本身就是一条系统级集成测试路径。

### 5.2 建议重点

- 先验证镜像能启动并打印监听地址
- 再用 `curl` 或浏览器做一次基本访问
- 若关注并发烟测，可再跑一次 `ab -n 5000 -c 20`

### 5.3 更适合它的验证方式

对该示例来说，比覆盖率更重要的是确认：

- 多任务已正确装配
- 网络监听与 `accept()` 路径可用
- 每连接线程能正常创建和退出
- 固定响应页能稳定返回

## 6. 跨项目定位

### 6.1 ArceOS

`arceos-httpserver` 是 ArceOS 的示例应用，用来展示“应用如何通过 `axstd` 获取线程和网络能力”，并验证相关 feature 的联合装配。

### 6.2 StarryOS

当前没有看到 StarryOS 直接复用这个示例。StarryOS 更关心系统调用与兼容层行为，而不是运行这个 ArceOS 风格的最小 HTTP 示例。

### 6.3 Axvisor

当前没有看到 Axvisor 与该示例有直接关系。它的用途主要局限在 ArceOS 侧的示例与集成验证。
