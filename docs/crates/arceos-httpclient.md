# `arceos-httpclient` 技术文档

> 路径：`os/arceos/examples/httpclient`
> 类型：二进制 crate
> 分层：ArceOS 层 / 系统行为样例
> 版本：`0.1.0`
> 文档依据：`Cargo.toml`、`src/main.rs`、`docs/arceos-internals.md`

`arceos-httpclient` 是一个极简的 ArceOS 网络示例。它通过 `axstd::net::TcpStream` 连接到 `ident.me`，发送一条固定的 HTTP GET 请求，然后把收到的首段响应打印到控制台。这个 crate 的真实价值不是提供 HTTP 客户端能力，而是用最短路径把“ArceOS 运行时 + `axstd` 网络接口 + TCP 连接 + 可选 DNS”这一整条系统链路跑通。

最关键的边界是：`arceos-httpclient` 只是系统行为样例，不是 HTTP 客户端库，也不是仓库里的网络基础设施。

## 1. 架构设计分析

### 1.1 设计定位

从源码可以直接看出，这个示例几乎没有自己的抽象层：

- `DEST` 决定目标地址
- `REQUEST` 固定写死最小 HTTP 请求头
- `client()` 负责解析地址、建立连接、发送请求、读取一次响应
- `main()` 只负责打印提示并调用 `client()`

它的职责非常纯粹：验证应用视角下的网络能力是否可用，而不是演示一个完整 HTTP 协议栈。

### 1.2 与 `axstd` 的关系

`Cargo.toml` 中只有一个可选依赖：

```toml
axstd = { workspace = true, features = ["net"], optional = true }
```

源码也用 `extern crate axstd as std` 的形式把 `axstd` 伪装成 `std`。这意味着：

- 示例完全站在应用视角，使用的是 `std::net` 风格 API
- 它并不直接触碰 `axnet`、`axnet-ng` 或 `smoltcp`
- 应用只声明“我要 `net`”，底层网络装配由运行时和内核负责

这正是 ArceOS 应用模型的典型样子。

### 1.3 `dns` feature 的真实作用

该示例只有一个显式 feature：

- `dns = ["axstd?/dns"]`

它只影响目标地址的表达方式：

- 开启 `dns`：目标是 `ident.me:80`
- 不开启 `dns`：目标退化为固定 IP `65.108.151.63:80`

也就是说，`dns` feature 的意义不是给示例增加“高级 HTTP 能力”，而是顺带验证主机名解析这条系统路径是否已经装配成功。

## 2. 核心功能说明

### 2.1 执行流程

`client()` 的执行顺序非常短：

1. 调用 `DEST.to_socket_addrs()` 并打印解析出的地址
2. 调用 `TcpStream::connect(DEST)` 建立 TCP 连接
3. `write_all()` 发送固定 HTTP 请求
4. 只调用一次 `read()` 读取最多 2048 字节
5. 假定返回内容是 UTF-8 并直接打印

### 2.2 这个示例实际验证了什么

虽然代码很少，但它一次性串起了几条关键系统路径：

- `axstd::net` 是否工作正常
- TCP 客户端连接是否能建立
- `Read` / `Write` 是否能贯通到底层网络栈
- 可选 DNS 解析是否生效

因此它更适合作为 smoke test，而不是 HTTP 协议测试框架。

### 2.3 明确没有做的事情

源码里已经留下注释说明“更长响应需要处理 TCP package problems”。这意味着该示例刻意没有处理：

- 循环读取或分块读取
- 完整 HTTP 报文解析
- keep-alive、chunked、重定向、连接复用
- 重试、超时与更复杂错误恢复

它验证的是“系统能跑一条 HTTP 风格 TCP 文本流量”，而不是“HTTP 客户端语义已经完备”。

### 2.4 关键边界

- 该示例不直接依赖 `axnet` / `axnet-ng` API
- 该示例不提供可复用 HTTP 抽象
- 该示例的输出能证明系统链路可用，但不能证明应用层 HTTP 语义完整

## 3. 依赖关系

### 3.1 直接依赖

| 依赖 | 作用 |
| --- | --- |
| `axstd`（可选） | 提供 `TcpStream`、`ToSocketAddrs`、`io` 等应用接口 |

### 3.2 间接依赖链

一旦启用 `axstd` 的 `net` 能力，该示例会间接拉起：

- `arceos_api`
- `axfeat`
- `axruntime`
- `axnet` 或 `axnet-ng`（取决于最终镜像装配）
- 更底层的驱动、调度和内存子系统

因此，它实际上是在验证整条 ArceOS 网络装配链，而不是单一 crate 的孤立行为。

### 3.3 跨层关系

| 层次 | 角色 |
| --- | --- |
| `arceos-httpclient` | 固定请求 + 最小示例入口 |
| `axstd` | 向应用暴露 `std::net` 风格接口 |
| `arceos_api` / `axruntime` | 把网络能力装进镜像 |
| `axnet*` | 提供真正的内核网络实现 |

## 4. 开发指南

### 4.1 运行方式

```bash
cargo xtask arceos run --package arceos-httpclient --arch riscv64 --net
```

若要连带验证 DNS，再启用对应 feature 或选择支持该能力的构建配置。

### 4.2 修改时的建议

1. 若目标只是验证 TCP 基本连通性，应保持它尽量简单。
2. 若想验证更复杂的 HTTP 语义，建议另建专门示例，不要把这个 smoke path 演化成真正客户端。
3. 若改成循环读、带超时或复杂请求头，应明确这是在扩展测试面，而不是在修改网络基础设施。

### 4.3 高风险点

- 域名解析路径是否真的被装配进当前镜像
- `TcpStream::connect` 的错误分支在目标平台上是否可观测
- 单次 `read()` 的简化逻辑很容易掩盖分段响应问题

## 5. 测试策略

### 5.1 当前测试形态

这个 crate 没有独立单元测试或集成测试。它自身就是一条系统级 smoke path。

### 5.2 建议重点

- 至少验证一次直接 IP 连接
- 若启用 `dns`，再验证一次主机名解析路径
- 关注串口输出中是否真的出现响应正文，而不是只看“连接成功”

### 5.3 更适合它的验证方式

对这个示例而言，比覆盖率更重要的是确认：

- 镜像能够启动到应用入口
- 网络栈已被正确装配
- 应用侧 `axstd::net` 行为基本成立

## 6. 跨项目定位

### 6.1 ArceOS

`arceos-httpclient` 是 ArceOS 的示例应用，用来展示“应用如何通过 `axstd` 使用网络能力”。它属于上层样例，不是网络基础设施本身。

### 6.2 StarryOS

当前没有看到 StarryOS 直接复用这个示例的证据。StarryOS 若要验证网络能力，通常会走自己的用户态或系统调用测试链，而不是运行这个 ArceOS 示例。

### 6.3 Axvisor

当前没有看到 Axvisor 与该示例有直接关系。它的意义主要局限在 ArceOS 应用与系统装配验证场景中。
