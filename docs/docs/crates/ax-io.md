# `axio` 技术文档

> 路径：`components/axio`
> 类型：库 crate
> 分层：组件层 / 通用同步 I/O 语义层
> 版本：`0.3.0-pre.1`
> 文档依据：`Cargo.toml`、`README.md`、`src/lib.rs`、`src/read/mod.rs`、`src/write/mod.rs`、`src/seek/mod.rs`、`src/buffered/*`、`src/iobuf/*`、`src/utils/*`、`tests/*`

`axio` 是仓库里所有“同步 I/O 行为”共享的一层公共协议。它把 Rust `std::io` 的核心 trait、缓冲包装器和若干辅助适配器移植到 `no_std` 语境，并将错误统一到 `ax-errno`。文件、socket、内存游标、用户态缓冲区访问器、POSIX 兼容层里的读写对象，都通过它来表达“可读”“可写”“可 seek”这些共同语义。

最需要先钉死的一条边界是：`axio` 只定义同步 I/O 接口与默认行为，不负责等待、唤醒、超时和多路复用。`PollState` 只是一个轻量就绪快照，不是事件系统。

## 1. 架构设计分析

### 1.1 设计定位

`axio` 解决的不是文件系统、网络协议或设备驱动本身，而是这些子系统都必须共享的 I/O 契约：

- 统一的同步 trait：`Read`、`Write`、`Seek`、`BufRead`
- 统一的缓冲包装器：`BufReader`、`BufWriter`、`LineWriter`
- 统一的游标与 glue 工具：`Cursor`、`copy`、`read_fn`、`write_fn`、`empty`、`sink`、`repeat`、`take`、`chain`
- 统一的“剩余长度”扩展：`IoBuf` / `IoBufMut`

这让上层代码在不关心底层对象究竟是文件、socket、内存切片还是用户缓冲访问器的前提下，复用同一套读写与缓冲逻辑。

### 1.2 模块划分

| 模块 | 作用 |
| --- | --- |
| `read` | 定义 `Read` / `BufRead`，提供 `read_exact`、`read_to_end`、`read_to_string`、`read_until`、`lines` 等默认实现 |
| `write` | 定义 `Write`，提供 `write_all`、`write_fmt` 等通用逻辑 |
| `seek` | 定义 `Seek` / `SeekFrom`，提供 `stream_len`、`stream_position`、`rewind`、`seek_relative` |
| `buffered` | 实现 `BufReader`、`BufWriter`、`LineWriter` 以及 `IntoInnerError` |
| `iobuf` | 定义 `IoBuf` / `IoBufMut` 及其扩展 trait，用于暴露剩余可读/可写长度 |
| `utils` | 放置 `Cursor`、`copy`、`take`、`chain`、`empty`、`sink`、`repeat`、`read_fn`、`write_fn` 等适配器 |
| `prelude` | 重导出常用 trait，供上层直接 `use ax_io::prelude::*` |

### 1.3 与 `std::io` 的关系

`README.md` 已明确说明：`axio` 基本沿用 Rust 标准库 `std::io` 的设计与大量实现细节，但为了适配内核和 `no_std` 场景做了几处关键收敛：

- 错误类型改为 `ax_errno::AxError`
- 不提供 `IoSlice`、`IoSliceMut` 与 `*_vectored` 系列接口
- 在不启用 `alloc` 时保留一条更适合固定缓冲区的最小能力路径

因此，`axio` 更准确的表述不是“重新发明一套 I/O 接口”，而是“给 ArceOS/StarryOS 提供可在 `no_std` 中使用的 `std::io` 内核版”。

### 1.4 `IoBuf` / `IoBufMut` 的真实意义

这两个扩展 trait 很容易被忽略，但在仓库里它们恰好体现了 `axio` 的定位：不仅要抽象“能不能读写”，还要在对象本身能给出长度信息时，把这类信息向上层显式暴露。

- `IoBuf::remaining()`：当前还剩多少字节可读
- `IoBufMut::remaining_mut()`：当前还剩多少空间可写

这不是新的 I/O 模型，而是给通用 trait 层补上一点对内核路径很实用的容量语义。`ax-net-ng` 的发送/接收接口、部分文件与缓冲区适配器都会消费这类信息。

### 1.5 `alloc` feature 的边界

`alloc` 是 `axio` 最关键的编译期开关。开启后会额外获得：

- `Read::read_to_end`、`Read::read_to_string`
- `BufRead::read_until`、`read_line`、`split`、`lines`
- 对 `Vec<u8>`、`Box<T>` 等分配型容器的 trait 实现
- 更完整的 `BufReader` / `BufWriter` 容量行为

不开启 `alloc` 时，`axio` 仍然是完整可用的同步 I/O 抽象，只是更偏向固定切片、固定容量缓冲和内核内部对象之间的最小通路。

### 1.6 与 `axpoll` 的关系

`src/lib.rs` 中的 `PollState` 只有两个布尔位：`readable` 与 `writable`。它的职责仅仅是表达“此刻是否可读/可写”，给上层留下一个统一的 readiness 结果结构。

真正的等待与唤醒不在 `axio` 中：

- `axpoll` 负责 `IoEvents`、`Pollable` 和 waker 集合
- `ax-task` 负责把 nonblocking I/O 桥接成 future
- `select` / `poll` / `epoll` 一类系统接口在更高层实现

这条边界对理解 `axio` 极其关键：`axio` 是同步 I/O 语义层，不是事件轮询层。

## 2. 核心功能说明

### 2.1 主要能力

- 为内核对象提供一套统一的同步 I/O trait
- 为大量常见场景提供默认实现，统一处理 EOF、短读、短写、缓冲扩容与格式化输出
- 提供 `BufReader` / `BufWriter` / `LineWriter` 等可复用缓冲器
- 提供 `Cursor`、`take`、`chain`、`copy` 等组合工具，减少上层 glue 代码
- 通过 `IoBuf` / `IoBufMut` 在有条件时向上层暴露剩余长度信息

### 2.2 典型上层调用关系

仓库里的真实调用链大致分为四类：

1. `ax_std::io` 直接重导出 `axio` 的 trait 与类型，把它包装成 ArceOS 应用看到的 `std::io` 风格接口。
2. `ax-fs`、`ax-fs-ng` 的文件对象实现 `Read` / `Write` / `Seek`，复用统一的缓冲器和默认读写逻辑。
3. `ax-net`、`ax-net-ng` 以及更上层 socket 封装使用 `axio` 作为同步收发 trait 的公共接口。
4. `ax-api`、`ax-posix-api`、StarryOS 的 `FileLike`/用户缓冲访问对象通过 `axio` 收敛系统调用读写路径。

### 2.3 `PollState` 的实际使用位置

虽然 `PollState` 本身很小，但它在旧一代同步接口里承担了一个稳定契约：

- `ax-net` 的 TCP/UDP socket 会返回 `PollState`
- `ax-api` 把它重导出为 `AxPollState`
- `ax-posix-api` 的 `fd_ops`、`pipe`、`stdio`、`fs`、`net` 等路径都在使用它

因此，`PollState` 不该被理解成“顺手放在这里的小结构体”，而应被视为同步 I/O 层与更高层轮询语义之间的兼容桥。

### 2.4 关键边界

- `axio` 不关心对象属于文件、socket、pipe、TTY 还是内存；它只定义同步 I/O 语义
- `axio` 不负责等待事件，也不直接注册 waker
- `axio` 不提供 POSIX fd、系统调用协议或 socket 状态机
- `axio` 不重新定义应用接口；应用通常通过 `ax_std::io` 间接接触它

## 3. 依赖关系

### 3.1 直接依赖

| 依赖 | 作用 |
| --- | --- |
| `ax-errno` | 定义 `Error`、`ErrorKind` 与 `Result` |
| `heapless` | 支撑无堆环境下的部分固定容量实现 |
| `memchr` | 支撑 `BufRead::read_until`、`skip_until` 等基于分隔符的扫描逻辑 |

### 3.2 主要消费者

直接或间接依赖 `axio` 的关键模块包括：

- `ax-std`、`ax-libc`
- `ax-api`、`ax-posix-api`
- `ax-fs`、`ax-fs-ng`
- `ax-net`、`ax-net-ng`
- StarryOS 的文件、网络、pipe、用户态缓冲访问和系统调用包装层

### 3.3 跨层关系

| 层次 | 与 `axio` 的关系 |
| --- | --- |
| `ax_std::io` | 面向 ArceOS 应用的重导出接口 |
| `ax-fs*` | 把文件对象映射到统一同步 I/O trait |
| `ax-net*` | 把 socket 收发路径映射到统一同步 I/O trait |
| `ax-posix-api` | 把系统调用中的文件描述符读写逻辑落到统一 trait 上 |
| StarryOS `FileLike` | 借助 `axio` 统一内核对象读写/seek 语义 |

## 4. 开发指南

### 4.1 依赖方式

```toml
[dependencies]
ax-io = { workspace = true }

# 需要字符串 / Vec 相关能力时：
# ax-io = { workspace = true, features = ["alloc"] }
```

### 4.2 为新对象接入 `axio` 的建议

1. 只实现真实需要的 trait，不要为了“接口完整”而硬塞 `Seek` 或 `BufRead`。
2. 如果对象天然知道剩余数据量或剩余容量，优先补上 `IoBuf` / `IoBufMut`。
3. 如果对象会暂时不可用，应返回 `AxError::WouldBlock` 或 `Interrupted`，不要自行发明错误语义。
4. 如果对象内部已经有缓存，再考虑实现 `BufRead`；否则交给 `BufReader` 等包装器更合适。

### 4.3 修改实现时的高风险点

- `read_to_end` / `read_to_string` 同时涉及 EOF 语义、增长策略和错误回滚
- `append_to_string` 一类路径必须保证 UTF-8 校验失败时不会留下脏状态
- `BufWriter::into_inner` / `IntoInnerError` 关系到错误恢复语义
- `Seek` 的默认方法会隐含依赖当前位置和长度一致性，尤其容易受包装器影响
- `alloc` 与非 `alloc` 两条路径都必须保持行为边界一致

## 5. 测试策略

### 5.1 当前已有测试资产

`components/ax-io/tests` 已有多组集成测试，核心文件包括：

- `buffered.rs`
- `copy.rs`
- `cursor.rs`
- `impls.rs`
- `io.rs`
- `iobuf.rs`
- `iofn.rs`
- `utils.rs`

这些测试覆盖了缓冲器、游标、容量语义、trait 默认实现、UTF-8 行为、短读短写和一批历史回归点，是 `axio` 最重要的回归资产。

### 5.2 建议重点

- EOF、短读、短写、`WriteZero`、`Interrupted`、`WouldBlock`
- `read_to_string` 遇到非法 UTF-8 时的回滚行为
- `BufReader` / `BufWriter` 的缓冲失效与错误恢复
- `alloc` 开关两条路径的行为一致性
- 涉及 `PollState` 语义时，至少补一条上层集成验证

### 5.3 推荐验证命令

```bash
cargo test -p axio
cargo test -p axio --features alloc
```

## 6. 跨项目定位

### 6.1 ArceOS

在 ArceOS 中，`axio` 是同步 I/O 语义基座。`ax-std` 负责把它包装成应用接口，`ax-fs*` 和 `ax-net*` 则负责把具体内核对象接到这层协议上。

### 6.2 StarryOS

StarryOS 大量直接复用 `axio`。文件、pipe、socket、用户缓冲访问对象都需要借它统一读写与 seek 语义，因此它在 StarryOS 中仍然是公共基础设施，而不是某个子系统的私有库。

### 6.3 Axvisor

当前没有看到 Axvisor 把 `axio` 作为独立 hypervisor 接口来直接设计策略的证据。它在 Axvisor 场景中更多是经由 ArceOS 公共层被间接复用，角色仍然是公共 I/O 抽象而非虚拟化策略层。
