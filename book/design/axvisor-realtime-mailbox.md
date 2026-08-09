# Axvisor 实时 CPU ↔ Host Mailbox 设计

本文是 [`axvisor-realtime-cpu.md`](axvisor-realtime-cpu.md) 的配套设计，覆盖 RT 执行域与 host（Axvisor / StarryOS）之间的双向、类型化、有界通信通道。属于[高风险功能](../guideline/feature-development.md)（新增跨核并发模型、公共 API、跨镜像 `#[repr(C)]` 布局），因此在实现前给出可独立评审的设计材料。

## 1. 问题与目标

RT 执行域此前只有单向、非结构化的对外通道：`ax-rt` 的 `output.rs`（1024B 字节环，RT 写、host 读）加只读 `status()` 快照。**host 无法向 RT 发送任何东西**，RT 也无法回报结构化事件。

Mailbox 提供 host 与 RT 之间**双向、类型化、有界**的控制消息通道：

- host → RT：下发命令 / 配置（调整参数、触发作业、切换模式）。
- RT → host：上报事件 / 结果（作业完成、越限告警、遥测）。

成功标准（可观察）：host 发一条带 tag+payload 的消息，RT 收到并读到一致内容；RT 回一条，host 读到一致内容——跨物理核端到端通过；队列满时返回 `Full` 而不阻塞、不丢已入队消息；RT 侧操作全部有界非阻塞。

## 2. 关键约束

Mailbox 与 `RtMutex`/`RtSemaphore` 本质不同：后者是**单核协作式**原语（假设 RT 核上无真正并发，靠 yield 回执行器阻塞）。Mailbox 是**真正的跨核共享内存**通信：

- **绝不用 `RtMutex`/`RtSemaphore` 保护 mailbox**——它们靠 yield 回 RT 执行器阻塞，host 侧没有 RT 执行器，会死锁且不健全。
- RT 侧操作必须 **lock-free、非阻塞、有界**（`try_send`/`try_recv`），RT 绝不 spin 等 host。
- 复用 `output.rs` 已验证的健全模型：**SPSC 环 + 索引作为唯一同步点**（生产者写完 payload 后以 Release 发布 write 索引，消费者以 Acquire 观察；消费者以 Release 前推 read，生产者以 Acquire 观察后才复用槽）。

## 3. 两层架构

```
┌─ 数据面（ax-rt 通用，无平台细节）──────────────┐
│  host→RT ring / RT→host ring  (SPSC, 共享内存)   │
├─ 通知面（平台能力，注入）──────────────────────┤
│  MailboxDoorbell::ring()  —— "踢"对端核触发中断  │
│  *_on_doorbell()          —— ISR 里置单原子 PENDING│
└────────────────────────────────────────────────┘
```

### 3.1 数据面

- 两条独立 SPSC 环：`to_rt`（host 产、RT 消费）、`to_host`（RT 产、host 消费）。
- 消息 `RtMessage`：`tag: u32` + `len: u16` + `payload: [u8; 48]`，值语义栈上拷贝。
- 环存储 `#[repr(C)]`，带 `magic`/`version`/`payload_cap`/`ring_slots` 头。这样在 AMP 部署（RT 侧为独立镜像）中两端镜像可对齐同一布局并检测不匹配。
- 满 → 返回 `RtMailboxError::Full` 并累加 `dropped` 计数，不覆盖。

### 3.2 通知面（开放接口）

```rust
pub trait MailboxDoorbell: Sync {
    /// 向对端核触发一次中断，非阻塞。
    fn ring(&self);
}
pub fn set_rt_doorbell(&'static dyn MailboxDoorbell);   // 敲 RT 核（host→RT 后）
pub fn set_host_doorbell(&'static dyn MailboxDoorbell);  // 敲 host 核（RT→host 后）
pub fn rt_mailbox_on_doorbell();     // RT 核 doorbell ISR 调用：置 rt_pending
pub fn host_mailbox_on_doorbell();   // host 核 doorbell ISR 调用：置 host_pending
```

`MailboxDoorbell` 是平台能力边界，backend 由集成层注入。接收侧的 IRQ handler 由平台实现，**只做**：解除硬件 assert + 调 `*_on_doorbell()`（单原子 store）。**禁止**在 ISR 里分配 / 映射内存 / 取可睡眠锁 / 关中断源。

### 3.3 唤醒模型

- 轮询兜底（无 doorbell / 非 aarch64）：RT 任务轮询 `rt_mailbox_recv()`；host 轮询 `host_mailbox_recv()`。`*_take_pending()` 作为可选优化位。
- doorbell（已在 QEMU aarch64 落地，见 §6.2）：`ring()` 触发 GIC SGI → 对端 ISR 置 pending → 消费者被唤醒后 drain。当前消费侧仍保留轮询循环，doorbell 是叠加的低延迟通知而非唯一唤醒源；executor WFI 空闲待后续。

## 4. 部署模型（同一 mailbox API，两种落地）

| | 模型 A：保留 SMP 核（当前 Axvisor） | 模型 B：大小核 AMP（SG2002） |
|---|---|---|
| RT 侧 | 同一 axruntime 的保留 secondary CPU | little 核独立 ax-rt 镜像 |
| host 侧 | 同镜像 host 核 | big 核 Linux/Starry 镜像 |
| 地址空间 | 共享 | 各自独立，仅共享 carveout |
| 一致性 | cache 一致 SMP | 可能非一致，用 `iomap`(uncached) carveout 规避 |
| 通知 | GIC SGI / RISC-V IPI | HW mailbox doorbell |
| 启动 | `secondary_cpu_owner` 分流 | little 核独立引导 |

## 5. SG2002 prior art（yfblock/tgoskits `oscomp`）

真机（C906B 大核跑 StarryOS + C906L 小核跑 bare-metal）已打通大小核邮箱，是本设计的 prior art。它验证了两层拆分（HW mailbox 只是 doorbell，消息走 DRAM 共享结构），并提供具体协议与踩坑教训。

传输协议（来自 `os/StarryOS/kernel/src/pseudofs/dev/cvi_mailbox.rs`）：

| 项 | 值 |
|---|---|
| HW mailbox regs | PA `0x0190_0000`, ctx buffer offset `0x400` |
| 触发 | 写 slot payload → `cpu_mbox_en[接收方CPU]` → 全局 `mbox_set` |
| CPU 号 | 大核=1, 小核=2；slot 分向：S2B=0, B2S=1（必须错开，mbox_set 全局） |
| PLIC source | 大核=101（dts `rtos_cmdqu`）, 小核=61 |
| DRAM 邮箱 | `#[repr(C)]` @ `0x9004_0000`, magic `0xC906C906`（跨镜像 ABI） |

固化为 doorbell backend 硬性契约（真机血的教训）：

1. **ISR 注册前地址必须稳定**（`into_raw` 固定，勿用按值返回的 `Arc::new(new())`）；野指针 → 无法 deassert → level-triggered PLIC 无限重投 → 核被打死。ax-rt 的 crate 静态天然满足。
2. **ISR 只做一件原子事**：禁止 iomap/log/取锁/分配。对应 `*_on_doorbell()` 的单原子设计。
3. **ISR 里不要 disable 中断源**：PLIC 对未使能 source 忽略 complete，会永久卡在 in-service。
4. level-triggered 必须正确 deassert；零长度不发消息。

复用而非重造：ax-rt 提供**通用**机制；`cvi_mailbox.rs` 是该模式的**专用实例**（相机/JPU 语义）。SG2002 backend 复用其传输契约（doorbell 协议、PLIC 101/61、`#[repr(C)]` 布局、ISR 纪律），不复用其消息 schema。

## 6. 已实现（QEMU aarch64）

- `components/ax-rt/src/mailbox.rs`：数据面（两条 SPSC 环 + `RtMessage` + `#[repr(C)]` header）+ 通知接口（`MailboxDoorbell` + `*_on_doorbell` + `*_take_pending` + doorbell 注册）+ `rt_mailbox_stats()`。
- Axvisor glue（`os/axvisor/src/realtime.rs`）：`mbox-echo` RT 服务任务（drain host→RT 命令并回显为 RT→host 事件）；host 侧 round-trip 自检并入 `log_priority_test_result`。
- Shell（`rt.rs`）：`rt status` 增加 mailbox 行；`rt send <text>` / `rt recv`。
- 通知面已从纯轮询升级为 **GIC SGI 双向 doorbell**（模型 A），详见 §6.2；消费侧仍保留轮询循环作为兜底，doorbell 是叠加的低延迟唤醒。

### 6.1 GIC SGI 双向 doorbell（模型 A）

两个方向各占一条专用 SGI，避开调度器 IPI：

| SGI | 方向 | 发起核 → 目标核 | 触发点 |
|---|---|---|---|
| 0 | — | — | 调度器 IPI（`ipi_irq()`，勿复用） |
| 1 | host → RT | host 核 → 保留的 RT 核 | `host_mailbox_send()` 后 `RtCoreDoorbell::ring()` |
| 2 | RT → host | RT 核 → host 引导核 | `rt_mailbox_send()` 后 `HostCoreDoorbell::ring()` |

接线：RT 核在 `ax_realtime_secondary_main` 里 `setup_rt_mailbox_doorbell()` 注册 SGI 1 的 per-CPU handler；host 引导核在 `main()` 里 `setup_host_mailbox_doorbell()` 注册 SGI 2 的 per-CPU handler。两侧 handler 都只调 `*_on_doorbell()`（单原子置 pending），符合 §3.2 的 ISR 纪律。

两条实现踩坑（已固化为契约）：

1. **GIC 域号必须运行时取，不能用 `AARCH64_GIC_DOMAIN` 兼容常量**：GIC IRQ 域号在启动时动态注册，兼容常量（`IrqDomainId(3)`）通常不等于真实域号，`is_gic_domain()` 会拒绝它，导致 `request_percpu_irq` / `send_ipi` 返回 `InvalidIrq` 而静默退回轮询。正确做法是借用运行时 IPI 的域：`ipi_irq().domain`。
2. **RT 核绝不碰共享 console 锁**：doorbell 的日志/观测一律放在 host 侧。被隔离的 RT 核若在发送路径里 `info!`（去抢 host 持有的 console 自旋锁）会把整机卡死。RT 侧 `ring()` 只发 SGI，反向 IPI 由 host 收到后打印。

### 6.2 验证

| 声明 | 层级 | 通过条件 | 状态 |
|---|---|---|---|
| host↔RT 双向 round-trip 内容一致 | QEMU `AX_RT_CPU=3 --smp 4` | `[RT mailbox test] host->RT->host round-trip PASS` | ✅ 已通过 |
| 双向 doorbell IPI 真实送达（非轮询兜底） | 同上 | round-trip 日志 `rt_notifications=1, host_notifications=1`；两行 `doorbell IPI` 分别记录 host→RT 与 RT→host | ✅ 已通过 |
| 默认关闭无回归 | build/QEMU | 不设 `AX_RT_CPU` 行为不变 | ✅ |
| 环满背压 | 后续 | `Full` 且不丢已入队 | 待补 |

## 7. 后续（SG2002 / 背压 / WFI）

- **模型 A IPI 已落地**（见 §6.1）：RT 核专用 SGI 1、host 核专用 SGI 2、双向送达已在 QEMU 验证。注意这放宽了 RT 核"不进普通 IRQ" 的隔离不变量——RT 核仅使能这一条 doorbell SGI，调度器 timer/IPI 仍只登记在 host 核集。
- **模型 B SG2002**：`Sg2002MailboxDoorbell` backend + carveout 映射 + 与 Starry 侧 `cvi_mailbox` 对齐 `#[repr(C)]` ABI；真机验证 `notify==消息数` 1:1。
- **背压测试** 与 **executor WFI 空闲**（当前消费侧仍轮询，doorbell 只做低延迟叠加，尚未据其进入 WFI）。

## 8. 非目标（v1）

MPSC 多 host 并发生产者、阻塞式 WFI 唤醒、零拷贝/大数据、消息优先级/确认重传、非一致内存的完整 cache 维护。均可后续独立叠加，不影响当前语义完整性。
