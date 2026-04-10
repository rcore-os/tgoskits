# `smoltcp-fuzz` 技术文档

> 路径：`components/starry-smoltcp/fuzz`
> 类型：模糊测试工作区 / 二进制目标集合
> 分层：组件层 / 协议栈质量保障工具
> 版本：`0.0.1`
> 文档依据：`Cargo.toml`、`fuzz/fuzz_targets/*`、`fuzz/utils.rs`

`smoltcp-fuzz` 不是协议栈本体，也不是系统镜像里会链接进去的运行时组件。它是围绕 `smoltcp` 组织的一组 `cargo fuzz` / libFuzzer 目标，专门用来把异常输入打到报文解析、repr/emit、TCP 头部健壮性、6LoWPAN 等高风险代码路径上。它的目标是发现 panic、越界、死循环、状态机异常和 round-trip 退化，而不是提供可复用 API。

最需要明确的边界是：`smoltcp-fuzz` 只是开发期质量保障工具，不参与 ArceOS、StarryOS 或 Axvisor 的运行时装配，也不应被理解成“网络测试应用”。

## 1. 架构设计分析

### 1.1 设计定位

`Cargo.toml` 中的 `package.metadata.cargo-fuzz = true` 已经明确表明：这是一个 `cargo fuzz` 工作区，而不是普通应用 crate。它的职责很单一：

- 为 `smoltcp` 提供可持续运行的 fuzz harness
- 按高风险输入面拆分出多个独立目标
- 把 fuzz 依赖与根工作区的日常构建隔离开

### 1.2 独立工作区设计

`Cargo.toml` 中同时存在：

- `[workspace] members = ["."]`
- 多个 `[[bin]]` fuzz 目标

这说明它刻意把自己从主工作区构建路径中隔离。这样做有两个直接好处：

1. `libfuzzer-sys` 等依赖不会污染普通 `cargo build` / `cargo test`
2. 每个 fuzz target 都能作为独立二进制交给 libFuzzer 驱动

### 1.3 目标拆分方式

当前共有 5 个 fuzz target：

| 目标 | 主要覆盖面 |
| --- | --- |
| `packet_parser` | 以太网帧 pretty print / parser 路径 |
| `tcp_headers` | 在 loopback + `Interface` / `SocketSet` 环境中定向扰动 TCP 头 |
| `dhcp_header` | DHCP 报文 parse -> repr -> emit round-trip |
| `ieee802154_header` | IEEE 802.15.4 头部 parse / emit |
| `sixlowpan_packet` | 6LoWPAN 及其下游多协议 parse / emit |

这种拆法不是按源码目录划分，而是按“高风险输入面”划分，更符合协议栈 fuzz 的实际需求。

## 2. 核心功能说明

### 2.1 每个 target 在测什么

#### `packet_parser`

`packet_parser.rs` 直接把任意字节串交给 `PrettyPrinter::<EthernetFrame<_>>::new("", data)`。它关心的不是网络连通，而是报文 pretty print / 浏览路径面对畸形输入时是否崩溃。

#### `dhcp_header` / `ieee802154_header`

这两个 target 都采用典型的 parse -> repr -> emit 结构：

- 能解析就尽量转成 repr
- 能转成 repr 就再 emit 回 buffer

这种模式非常适合发现“解析成功但无法稳定重建”的报文处理缺陷。

#### `sixlowpan_packet`

`sixlowpan_packet.rs` 是现有目标里最复杂的一项。它不仅覆盖 6LoWPAN 头本身，还会继续向下尝试 NHC、UDP、TCP、ICMPv4/ICMPv6、IPv6 扩展头等路径的 parse / emit。它瞄准的是一个高度分支化、极易因异常输入出错的协议面。

#### `tcp_headers`

`tcp_headers.rs` 并不是纯 parser fuzz。它会：

- 构建 loopback 设备和完整的 `Interface` / `SocketSet`
- 先把一条最小 TCP 连接跑起来
- 再对真实往返数据包的 TCP 头部进行定向扰动

这比“直接喂随机字节给 parser”更贴近“协议状态机 + 畸形头部”组合问题。

### 2.2 `utils.rs` 的作用

`fuzz/utils.rs` 基本复用了 examples 里的中间件工具代码，主要负责：

- 解析命令行选项
- 打开 pcap 输出
- 包装 `FaultInjector`、`Tracer`、`PcapWriter`
- 配置丢包、损坏、尺寸限制与速率限制

这说明 `smoltcp-fuzz` 并不是最小极简 harness，而是复用了协议栈已有的设备中间件生态来放大测试覆盖面。

### 2.3 关键边界

- `smoltcp-fuzz` 不验证 `ax-net` / `ax-net-ng` 的系统 socket 语义
- `smoltcp-fuzz` 不负责系统级连通性回归
- `smoltcp-fuzz` 主要关注解析健壮性、repr/emit 一致性以及部分协议状态机面对畸形输入时的鲁棒性

## 3. 依赖关系

### 3.1 关键直接依赖

| 依赖 | 作用 |
| --- | --- |
| `smoltcp` | 被测试对象 |
| `libfuzzer-sys` | libFuzzer 运行时 |
| `arbitrary` | 为复杂输入结构生成随机实例 |
| `getopts` | harness 工具与中间件选项解析 |

### 3.2 与 `smoltcp` 的关系

`smoltcp-fuzz` 不是业务消费者，而是 `smoltcp` 的质量保障外壳。它直接通过 `smoltcp = { path = ".." }` 依赖本仓库这份协议栈源码，因此 fuzz 发现的问题会直接对应到当前仓库实现，而不是外部发布版二进制。

### 3.3 跨系统关系

ArceOS / StarryOS 不会运行这些 fuzz target，但都会间接受益于修复结果，因为它们的 IP 协议行为都建立在同一份 `smoltcp` 之上。

## 4. 开发指南

### 4.1 运行方式

推荐方式是使用 `cargo fuzz`：

```bash
cd components/starry-smoltcp/fuzz
cargo fuzz run packet_parser
```

切换其他目标时，只需把 target 名称替换为 `tcp_headers`、`sixlowpan_packet` 等。

### 4.2 什么时候应补新 target

以下情况值得新增或强化 fuzz harness：

- 新增了新的 wire repr / packet parse 路径
- 修改了 TCP、DHCP、6LoWPAN 等高风险状态机分支
- 修复了由异常输入触发的 bug，希望把它永久固化成回归
- 增加了新的中间件、pretty printer 或报文组合路径

### 4.3 修改已有 target 时的注意点

- 优先保持 target 稳定、可长期运行，不要把业务逻辑塞得过重
- 对需要状态机环境的场景，优先采用 `tcp_headers.rs` 这种“先建立最小系统，再注入畸形数据”的模式
- 若 target 依赖 examples 工具代码，需同步注意两边行为是否漂移

## 5. 测试策略

### 5.1 当前覆盖重点

现有 5 个 target 已覆盖三类关键风险：

- parser / pretty printer 崩溃类问题
- repr / emit round-trip 一致性问题
- 协议状态机面对畸形 header 或异常输入时的健壮性问题

### 5.2 建议重点

- 新协议或新报文格式优先补最小 parse/emit fuzz
- 对状态复杂的逻辑，先构造最小运行环境，再做定向扰动
- 任何修复过的 parser panic、越界、死循环问题，都应尽量固化为 corpus 或新 target

### 5.3 与普通测试的分工

| 验证手段 | 更适合发现的问题 |
| --- | --- |
| 单元测试 | 预期输入下的功能正确性 |
| `netsim` / examples | 协议行为与性能回归 |
| `smoltcp-fuzz` | 非法输入、畸形报文、异常状态迁移下的健壮性 |

## 6. 跨项目定位

### 6.1 ArceOS

对 ArceOS 来说，`smoltcp-fuzz` 不是运行时组件，而是 `smoltcp` 底层协议引擎的开发期安全网。

### 6.2 StarryOS

对 StarryOS 也是一样。它不会直接运行 fuzz target，但会间接受益于这些 target 提前暴露出的解析与状态机缺陷。

### 6.3 Axvisor

当前没有看到 Axvisor 直接使用 `smoltcp` 网络路径，因此 `smoltcp-fuzz` 对它的影响最多是间接的公共代码质量提升，而不是直接的系统测试工具。
