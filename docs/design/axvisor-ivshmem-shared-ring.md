# axvisor-ivshmem：共享环形缓冲消息协议

状态：协议设计提案，不以当前实现作为约束或完成证明。

本文描述 ivshmem 共享内存设备上的多 peer ring 协议。核心目标是任意 VM 可以向指定 VM 发送消息，共享页数据访问不逐次经过 hypervisor，并尊重 ivshmem 的权限模型。

正文中的字段示意和伪代码只表达拟议协议，不对应具体源文件。面向应用的 `ivshmem_socket` 接口、端口/session、统一 IPC 缓冲及验收，以 [ivshmem_socket 设计](axvisor-ivshmem-socket.md) 为准。该提案由一个 guest 服务独占设备，通过 connect 建立 session，不同 session 的片段交错发布并独立重组；接收额度、控制消息与 CRC 使用拟议 Message V2。共享页访问、MMIO 通知和本地 IPC 是不同路径，本地 IPC 会使用 guest 系统调用。

## 权限模型约束

ivshmem BAR2 分为 State Table（全员只读）、可选的公共 R/W 区、以及每 peer 一个的 output section。output section 的权限是**单向**的：只有属主可写，其余 peer 只读。

这带来一条贯穿全设计的规则：

> 任何字节都由"拥有它的那一方"写入自己的 output section，对端只读。

因此传统单区域双 ring（head/tail 同处一个结构）的布局不可行：消费者无法回写生产者 ring 里的 `head`。解法是**索引分离**——生产者索引和消费者索引分别存放在各自的 output section 中，互读对方的。

## BAR2 分区与页布局

BAR2 按 4 KiB 页划分：State Table 占起始一页，可选 Common 区随后，各 peer 的 output section 按 peer id 顺序排列。本文选择每个 peer 一个 4 KiB output section，即一页容纳一个完整的发送 ring（含消费其他 ring 的 credit）。一个 64 KiB BAR2 在无 Common 时最多容纳 15 个这样的 peer；下面以包含一页 Common、三个 peer、每 peer 一页 output 的配置为例说明拟议布局。

```text
BAR2 page/slot  | 00  | 01  | 02  | 03  | 04  | 05  | 06  | 07  | 08  | 09  | 10  | 11  | 12  | 13  | 14  | 15  |
----------------+-----+-----+-----+-----+-----+-----+-----+-----+-----+-----+-----+-----+-----+-----+-----+-----+
p00 (state)     | ST  | ST  | ST  | ST  | ST  | ST  | ST  | ST  | ST  | ST  | ST  | ST  | ST  | ST  | ST  | ST  |
p01 (common)    | CM  | CM  | CM  | CM  | CM  | CM  | CM  | CM  | CM  | CM  | CM  | CM  | CM  | CM  | CM  | CM  |
p02 (peer 0)    | M0  | D00 | D01 | D02 | D03 | D04 | D05 | D06 | D07 | D08 | D09 | D10 | D11 | D12 | D13 | D14 |
p03 (peer 1)    | M1  | D00 | D01 | D02 | D03 | D04 | D05 | D06 | D07 | D08 | D09 | D10 | D11 | D12 | D13 | D14 |
p04 (peer 2)    | M2  | D00 | D01 | D02 | D03 | D04 | D05 | D06 | D07 | D08 | D09 | D10 | D11 | D12 | D13 | D14 |
p05..p15        | ... (reserved / unmapped) ...

ST  = state-table page (not protocol slots; read-only for all peers)
CM  = common page (not protocol slots; read/write for all peers)
M0  = peer 0 metadata slot (SectionHeader + credit[]); M1/M2 likewise
Dxx = data slot index within that peer's ring (D00..D14)
Output peer N = read/write for peer N, read-only for other peers
```

该图覆盖整个 64 KiB BAR2，省略号代表页 5..15；每个完整行是一个 4 KiB 页，每列对应页内 256B 范围，只有 output section 的列才按协议解释为 slot。页 0（`0x0000..0x1000`）是所有 peer 只读的 State Table，其中状态条目实际以 4B 为单位，并非 16 个协议 slot。页 1（`0x1000..0x2000`）是全员可写但本协议不使用的 Common。页 2、3、4 分别是 `Output(0)`（`0x2000..0x3000`）、`Output(1)`（`0x3000..0x4000`）和 `Output(2)`（`0x4000..0x5000`）；各页只有属主可写，其他 peer 只读。页 5..15（`0x5000..0x10000`）是未映射的 BAR2 Reserved，而非可供 ring 使用的空闲页。每个 output section 正好放 16 个 256B slot：第 0 个存 `SectionHeader` 和 `credit[max_peers]`，第 1..15 个存负载，没有单独的元数据页。部署必须明确选择每 peer 一页 output 的 profile，不能依赖某个实现的默认配置。metadata header 与 credit 合计不能超过 256B；以 15 个 peer 为例，5 个 32 位 header 字段和 15 个 32 位 credit 字段共 80B，其余空间仍在该 slot 内。

权限以整个 `Output(P)` 页为单位：P 可写自己的 metadata slot 和数据 slot，其他 peer 只能读。因此接收方不能在 P 的 ring 中更新 head，而应写自己 metadata slot 中的 `credit[P]`；P 读取对端的 `credit[P]` 才能回收负载。stage-2 映射必须按页体现 output 所有权（属主 RW、其他 peer R），BAR2 尾部 Reserved 页不映射；页内 slot 格式按本协议独立解释，不能从具体映射代码推断。

## 总体结构

一条 ivshmem link 上，每个 peer 的 output section 承载一条**发送 ring**（目的地任意，靠 slot 内的 dst 标记区分）和一个**信用区**（记录自己作为消费者对其他每个 peer ring 的消费进度）：

```text
Peer P 的 output section（4 KiB，P 写，其他 peer 只读）
┌──────────────────────────────────────────┐
│ slot 0: SectionHeader + credit[N]        │
│   magic / version / CAP=15 / tail        │ ← P 的生产者序号
│   credit[i] = 对 peer i ring 的 head     │ ← 跳过自己
├──────────────────────────────────────────┤
│ slot 1..15: 15 个数据 slot               │
│   { seq, dst, len, flags, payload ... }  │
└──────────────────────────────────────────┘
```

每个 VM 需要维护的状态只有两类，与 peer 数线性相关：

- **自己的 `tail`**：本地变量，写自己 section；
- **对每个发布者的 `head`**：消费进度，写进自己 section 的 `credit[pub]`，供对应发布者回收时读取。

寻址方式是链路层风格：发送方选择 dst 只是往 slot header 里填一个 peer id，**不需要为每对 VM 预建通道**，内存占用为每 peer 一条 ring。

## ABI 布局

共享控制字段和载荷的访问单元约定为对齐的 32 位原子量。每个 slot 固定 256B，metadata 占 1 个 slot，数据 ring 容量固定为 `CAP=15`。下面的类型仅用于示意字段关系，实际 ABI 以字节尺寸、对齐和访问契约为准，不由某种语言的结构体布局决定。

```rust
/// Metadata slot，位于每个 output section 起始的 256B。
#[repr(C, align(256))]
struct SectionHeader {
    magic: AtomicU32,          // "IVC3"
    version: AtomicU32,        // 协议版本（此布局需新版本）
    capacity: AtomicU32,       // 数据 slot 数：15
    slot_size: AtomicU32,      // 单个数据 slot 字节数：256
    tail: AtomicU32,           // 生产者下一条消息的序号，仅属主写
    credit: [AtomicU32; N],    // credit[i] = 本 peer 对 peer i ring 的下一待读序号
    // 填充到 256B；N 等于 link profile 的 max_peers
}

/// 单个 slot。
#[repr(C, align(256))]
struct Slot {
    seq: AtomicU32,            // 序列号，等于写入时的 tail 值
    dst: AtomicU32,            // 目标 peer id；DST_BROADCAST = 0xFFFF
    len: AtomicU32,            // payload 有效字节数
    flags: AtomicU32,          // 保留（消息类型等留给上层）
    payload: [u8; PAYLOAD],    // 定长载荷区
}
```

`seq` 使消费者能检测强制回收造成的丢步（见下文）；`dst` 是唯一的寻址信息。`SectionHeader`（含 credit）必须补齐到恰好 256B；其后的第一个数据 slot 从页内偏移 `0x100` 开始，数据 slot `i` 的页内偏移为 `(i + 1) × 256`。slot 以 256B 对齐，因而不会跨 4 KiB 页。数据 slot 的四个 32 位字段占 16B，余下最多 240B 用于 payload；消息头也占用 payload，正文片段上限为 240B 减去消息头长度；`ivshmem_socket` 提案选择 44B 消息头，因此正文片段最多 196B。

`CAP=15` 时不能直接让回绕的 `u32` 序号按 `% CAP` 定位：`2^32` 不能被 15 整除，回绕处会重复选中同一数据 slot。本协议将 `0xffff_ffff` 保留为未初始化的序号值，有效序号为 `0..0xffff_fffe`，下一个序号在 `0xffff_fffe` 后回到 0。循环长度 `M=0xffff_ffff` 能被 15 整除，因此 `seq % 15` 在回绕处仍指向下一个 slot。发送、接收、credit 和 slot 的 `seq` 均使用同一套序号规则。`seq_next(x)` 在 `x == M - 1` 时返回 0，否则返回 `x + 1`；`seq_distance(a, b)` 是从 a 前进到 b 的模 M 距离，即 `b >= a` 时为 `b - a`，否则为 `M - a + b`。反向移动的 `seq_sub` 也按模 M 计算。不能直接用普通大小比较、按模 2^32 回绕的减法或普通最小值判断新旧。长时间停滞且允许覆写的 peer 若落后整整一圈序号，单凭 32 位序号无法辨别新旧；必须在发生这种情况前重建 link 或增加代际信息。

## 发送流程（P → B）

```rust
let tail = self.tail;                 // 有效序号：0..0xffff_fffe
if self.max_ready_lag(tail) >= CAP {
    return Err(WouldBlock);          // 全局背压，见下节
}
let slot = &mut self.slots[tail as usize % CAP];
slot.write_payload(dst, bytes);      // 先写 payload/len/dst
slot.seq.store(tail, Release);       // 最后发布 seq
self.tail.store(seq_next(tail), Release);
doorbell(dst);                       // 可批量：多个 slot 合并一次
```

`doorbell` 只是通知 hint：发送方可以攒批，接收方也可以纯轮询。数据路径零 VM-exit（doorbell 本身是 MMIO 写，频率可控）。

### 通知策略：定向通知，不广播

全局最慢背压下一个自然的担忧：只通知 dst，其他 VM 不推进 head，最慢 peer 的 credit 迟早会被未通知者卡住。不需要为此广播 doorbell（每条消息唤醒全体是 O(N) 中断风暴），通知机制分三层：

1. **定向通知**：发送方只对 dst 发 doorbell（广播消息例外，通知全体 READY peer）；
2. **顺手清扫**：任何 VM 被唤醒处理消息时，顺带把**所有**发布者的游标扫一遍并回写各自 credit——成本只是读几个 `tail`，而 VM 本来就醒着。活跃 VM 的 head 因此始终接近最新，不会成为最慢者；
3. **背压催更**：发布者命中 `WouldBlock` 且发现最慢者 X 的 head 明显落后时，对 X 补发一个 doorbell（可用独立 MSI-X vector 或保留 flags 区分"有你的消息"与"请催更"）。这是罕见的按需事件，稳态下零开销。

一个从未收到消息、也从未被唤醒的 VM 不推进 head 是无害的——它只会成为 `min` 的下界，而催更机制保证发布者真正被它堵住时能唤醒它。纯轮询模式的 VM 则完全不依赖 doorbell。

### 阻塞的难得性与自愈性

“闲 VM 导致消费阻塞”需要同时满足：该 VM 在 ring 被填满（CAP 个 slot）的整个期间没有收到任何以它为 dst 的消息和广播、且不轮询。即使发生，后果也不是死锁：发布者 `WouldBlock` → 补发一次催更 doorbell → 该 VM 唤醒后顺手清扫所有游标，head 直接跳到当前位置，阻塞解除。代价是有界的一次额外中断和一次唤醒延迟，且只影响这一个发布者的 ring，不波及 link 上其他流量。

真正需要区别对待的是另一种情况：**VM 卡死但 state 仍是 READY**（hypervisor 尚未复位它）。此时催更无效，全局最慢背压意味着该发布者完全停写。这不是通知问题而是活性问题，处置策略按优先级：

1. 允许丢弃的通道：发布者超时后强制回收覆写，丢步由 seq 检测；
2. 不容许丢弃的通道：依赖 hypervisor 侧 watchdog 复位挂起的 VM（state 清零后自动退出 min 计算）；
3. 对背压隔离要求高的部署：升级为演进路径中的按 dst 追踪回收。

## 接收流程（B）

B 被 doorbell 唤醒（或轮询）后，对每个发布者 P：

```rust
let tail = P.tail.load(Acquire);
// 如允许强制覆写且 seq_distance(self.cursor[P], tail) > CAP，
// 先按序号距离统计丢步并将 cursor 移至 seq_sub(tail, CAP)。
while seq_distance(self.cursor[P], tail) > 0 {
    let cursor = self.cursor[P];
    let slot = &P.slots[cursor as usize % CAP];
    if slot.seq.load(Acquire) == cursor {           // 未被覆写
        if slot.dst == ME || slot.dst == BROADCAST {
            handle(slot);
        }
        // dst 不匹配的 slot：跳过，游标照样前进
    } else {
        stats.lost += 1;             // 被强制回收越过，丢步
    }
    self.cursor[P] = seq_next(cursor);
}
self.credit[P].store(self.cursor[P], Release);      // 回写自己的 credit
```

规则不变式：**head 只表达"之前的都可以回收"，不表达"我感不感兴趣"**。活着的 VM 永远不会因为"不想处理"而卡住回收。消费进度按模 M 递增，跳过的 slot 与消费过的 slot 对回收的影响完全相同。

## 回收：全局最慢背压（v1 简化策略）

发布者可写空间的计算：

```rust
fn max_ready_lag(&self, tail: u32) -> u32 {
    let mut lag = 0;
    for peer in peers where state_table[peer] == READY && peer != self {
        let head = peer.credit[self.id].load(Acquire);
        lag = lag.max(seq_distance(head, tail));
    }
    lag
}
// 可写条件：max_ready_lag(tail) < CAP；seq_distance 按模 M 计算
```

即**最慢的 READY peer 决定全组背压**。这是有意的简化：任何一个 READY 的 VM 停滞，最终都会阻止发布者继续写。

接受这个语义的理由：

- 协议最简单——发布者不需要记录每个环位的 dst，不需要按 dst 追踪回收；
- 配合"空闲 VM 也推进 head"的不变式，正常运行的 VM 不会成为最慢者；成为最慢者等价于"故障或严重过载"，此时停写是可接受的失败语义；
- 死亡检测不依赖 head：见下节。

对允许丢弃的通道（遥测、日志），发布者可以在 `WouldBlock` 时选择**强制回收**：直接覆写最旧 slot 继续前进。受害的消费者发现自己落后 `CAP` 以上时，先按序号距离统计丢步，再将游标移到 `tail` 前最多 `CAP` 个 slot；读取时还需核对 slot 的 `seq`，不能把已覆写的数据当作旧消息。强制覆写与并发读取的安全性仍需单独定义，不能只靠一次 `seq` 检查就保证载荷不被写者同时修改。

后续若需要隔离背压，可升级为"按环位 dst 追踪回收"（只有该环位上一条消息的 dst 需要读过），协议布局不变，纯发布者本地策略，向后兼容。

## 成员资格与生命周期

消费进度（head）与成员资格分离，后者复用 ivshmem 原生机制：

- **State Table**：peer 初始化好自己的 section header 与 credit 区后，写 `state = READY`；发布者只对 READY 的 peer 取 min。VM 复位或断开时 hypervisor 自动将其 State Table 条目清零，发布者下一拍即自动将其排除——**无需心跳，无需 syscall**；
- **加入**：新 peer 将每个发布者的 `credit[pub]` 初始化为当时的 `tail`（从最新消息开始），再置 READY；
- **退出**：先清 READY，再停止推进 head。

`idle / waiting / polling` 等调度状态是各 VM 的本地事务，不进入共享内存。

## 内存序

内存序由共享页的发布、观察和回收契约定义，不依赖某个队列实现：

- 发布者：写 slot 内容 → `Release` 存 `seq`/`tail`；doorbell 的 MMIO 写保证此前的数据写对接收方可见（ivshmem 规范要求）；
- 消费者：`Acquire` 读 `tail` 与 `seq` → 读 slot → 处理/跳过后 `Release` 存 credit；
- 发布者回收前 `Acquire` 读各 peer credit。

## 协议分层

共享 ring 只负责物理槽位、目的 peer、序号及回收，不决定应用连接和完整消息的语义。层间通过 payload 长度和发布顺序建立契约，不要求某个现有模块的接口或编码保持不变。

- metadata 描述 magic、version、容量及槽位大小，初始化时必须检查兼容性；
- 发送者维护自己 output 中的 tail 和 slots，消费者在各自 output 中维护 credit；
- 上层消息协议解释 payload 内的 session、message ID、分片标志和 CRC；
- `ivshmem_socket` 应用接口负责监听、连接和完整消息交付，不向应用暴露物理 ring 索引。

## 容量规划

每个 output section 固定一页：1 个 metadata slot 加 15 个数据 slot，恰好 `16 × 256B = 4096B`。64 KiB BAR2 扣除 State Table 的一页，无 Common 时最多支持 15 个 peer；如图保留一页 Common 时，最多支持 14 个 peer。图示配置明确选择三个 peer、一页 Common 和每 peer 一页 output，部署参数必须逐项核对，不采用隐含的默认布局。每条 ring 最多积压 15 个 slot；若消息需要多个 frame，实际可排队的完整消息更少。容量固定后，应检查最慢 READY peer 的容忍延迟与峰值速率能否在 15 个 slot 内消化；不满足时需要增加每 peer 的页数或拆分 link，而不是假定一页能覆盖所有负载。

## 演进路径

1. v1：本文协议（全局最慢背压，单播 dst + 广播）；
2. 按环位 dst 追踪回收，消除全局背压耦合（发布者本地策略，ABI 不变）；
3. 组播语义扩展：`dst` 位图或订阅组（保留 flags/扩展字段）；
4. 多 link 按角色隔离（控制面 / 数据面）。
