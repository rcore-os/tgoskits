# axvirtio-net 接入 AxVisor 计划（AArch64，确定性网络烟测）

> 本文承接 [`AXVIRTIO_NET_IMPLEMENTATION_PLAN.md`](./AXVIRTIO_NET_IMPLEMENTATION_PLAN.md)。
> `axvirtio-net` 的协议级实现已经完成；本文只规划 VMM/OS glue、guest 启动描述、
> IRQ 路由和端到端验证。

## 1. 目标与完成标准

在 QEMU AArch64 `virt,gic-version=3` 上启动 AxVisor 和单 vCPU ArceOS guest，guest
不直通宿主网卡，通过一个模拟的 virtio-mmio net 设备完成以下确定性流程：

1. guest 从 DTB 枚举 `virtio,mmio` net 设备并完成 feature/queue 初始化；
2. guest 向固定的虚拟 peer 发送 UDP 数据报；
3. AxVisor 后端生成 UDP echo 响应并交给 RX virtqueue；
4. guest 应用收到内容一致的响应；
5. 停止、重新 prepare/reset VM 后不会遗留 RX worker，也不会退回缺少 factory 的
   默认设备配置。

仅看到 probe、MMIO trap 或 TX 日志不算端到端完成。

## 2. 评审结论：原计划需要修正的问题

### 2.1 不能从 RX 线程直接写 `ICH_LR_EL2`

`virtualization/axvm/src/arch/aarch64/gic.rs::inject_interrupt` 操作当前物理 CPU 的
GIC virtualization system registers。RX worker 可能运行在任意 pCPU，而且调用时目标
vCPU 可能尚未 bind；直接写 LR 会注入错误的 vCPU 上下文，并且 LR 满时当前实现会
panic。

设备 IRQ 必须进入 AxVM 已有的 pending interrupt 路径：记录目标 vCPU、唤醒其任务并
发送 IPI，最终由 vCPU run loop 在正确上下文调用架构注入。不得把 LR 满处理放到网络
worker，也不得以直接调用 `gic::inject_interrupt` 作为 `IrqSink` 实现。

### 2.2 仅保存 stage-2 root 不能实现安全的 guest memory accessor

`AddrSpace::translate_and_get_limit` 不只查询页表，还依赖 `MemorySet` 中的 area 元数据；
stage-2 root paddr 无法复用这个方法。重新从 root 构造一个 owning page table 还会造成
页表所有权和 Drop 冲突。

此外，当前 `translate_and_get_limit` 返回整个 area 大小，而不是从传入 GPA 到 area
末尾的剩余长度。非 area 起点访问时会高估可访问范围。把返回的 HPA 在 VM 锁外直接
解引用还会与动态 map/unmap 竞争，并隐含 HPA 等于 HVA 的错误假设。

集成层应使用持有 `Weak<AxVM>` 的 accessor，并通过 AxVM 已有的
`read_from_guest`/`write_to_guest` 一类复制 API，在 VM resource 锁保护下完成翻译和
拷贝。不要缓存裸 root、AddrSpace 引用、HPA 或 guest slice。

### 2.3 wrapper/factory 属于 AxVisor glue，不属于通用 axvm

virtio-net wrapper 同时决定具体 backend、RX worker 和测试拓扑，这些是 AxVisor 的 OS
glue/runtime 策略。把它们放进 `axvm` 会让通用 VMM core 依赖特定网络运行时，也迫使
默认 VM prepare 猜测 backend。

通用能力（受保护的 guest memory copy、VM IRQ 排队）放在 `axvm`；
`VirtioNetDeviceAdapter`、factory、echo backend 和 worker 放在
`os/axvisor/src/virtio_net/`。`axdevice` 和 `axvirtio-net` 保持 OS-agnostic。

### 2.4 factory 回调加全局单值 slot 不满足生命周期要求

全局 `Once<Mutex<Option<_>>>` 无法区分 VM，reset 后会保留旧 device/IRQ，失败回滚时也
容易泄漏半初始化对象。factory 不应反向调用 AxVisor 保存 side channel。

设备注册后由 AxVisor 从该 VM 的 device registry 按具体 adapter 类型取得 runtime
endpoint，并以 `VMId` 为键管理 worker。worker 必须有取消/退出协议；VM stop、reset、
remove 和 prepare 失败都要清理对应 generation。

### 2.5 原样回灌 TX 以太帧不能证明应用层 loopback

原 TX 帧的目的 MAC/IP/UDP port 通常不是 guest 自己，原样放进 RX 后会被网络栈丢弃。
烟测后端应实现一个最小、确定性的虚拟 peer：响应 ARP，并对指定 IPv4/UDP 端口生成
地址、端口和校验和正确的 echo 帧。这样才能用普通 UDP socket 验证完整数据路径。

### 2.6 还缺少 vGIC、DTB 编码和 reset 约束

- `interrupt_mode = "emulated"` 本身不会保证 guest DTB 中有可用且与 emulated/GPPT
  设备一致的 GIC 节点；配置必须显式给出并先验证 timer IRQ。
- `interrupt-parent` 不能假设已有 phandle；`reg` 也不能固定编码成两个 32-bit cell。
  必须遵守父节点的 `#address-cells`/`#size-cells`，并验证 GIC
  `#interrupt-cells = <3>`。
- `AxVM::reset` 和 stopped VM 的 `start` 当前会再次走默认 `prepare()`。只在首次启动
  调用借用型 `prepare_with_factories` 会在 reset 后丢失 virtio-net factory 和 IRQ
  backend，因此 prepare 配置必须由 VM 持久保存，或所有重建入口统一由 AxVisor
  编排；不能只修首次启动。

## 3. 修订后的边界与数据流

```text
guest MMIO exit
  -> AxVmDevices
  -> VirtioNetDeviceAdapter                         [os/axvisor glue]
  -> VirtioMmioNetDevice                            [axvirtio-net]
  -> AxvmGuestMemoryAccessor -> Weak<AxVM>
       -> locked guest copy API                     [axvm]

guest TX notify
  -> DeterministicUdpEchoBackend::transmit          [os/axvisor runtime]
  -> validate Ethernet/ARP/IPv4/UDP, enqueue reply
  -> wake RX worker (never re-enter device in transmit)

RX worker
  -> device.receive_frame
  -> RxOutcome::Delivered
  -> IrqLine::pulse
  -> AxVmIrqSink -> queue interrupt for target vCPU [axvm runtime]
  -> vCPU run loop injects in bound AArch64 context
```

`NetworkBackend::transmit` 当前在 TX queue lock 内调用，因此 echo backend 只校验、构造
和入队，不得调用 `receive_frame`、IRQ 注入或任何可能等待 guest/device 锁的路径。

## 4. 分阶段实施

每个缺陷修复先添加一个在旧实现上必然失败的确定性测试，再实现修复并验证同一测试
通过。每阶段保持可编译，避免同时调试 guest memory、IRQ、FDT 和网络协议。

### 阶段 0：冻结配置契约和启动基线

- 新增专用 VM TOML，不修改现有 `arceos-smp1.toml`。
- 使用单 vCPU、`interrupt_mode = "emulated"`、固定 MMIO base/size、SPI INTID、guest
  MAC、peer MAC/IP 和 UDP echo port。
- `cfg_list` 首版明确为 6 个 MAC octet；factory 要求长度恰好为 6 且每项 `<= 255`，
  不接受静默截断或额外未定义字段。
- MMIO 区间要求页对齐、长度至少覆盖 transport 和 12-byte net config；SPI 要求
  `irq_id >= 32` 且不与 timer/其他设备冲突。
- 显式配置 guest 可用的 GICD/GICR emulation/GPPT，并提供与之匹配的 guest DTB。
  先在不添加 virtio-net 的情况下启动到 shell，并用 virtual timer 证明 IRQ 路径可用。
- 为 TOML 反序列化、MAC/IRQ/MMIO 校验和资源冲突添加 host 单测。

### 阶段 1：VM-backed guest memory capability

- 在 `axvm` 增加可克隆的 `AxvmGuestMemoryAccessor`，只保存 `Weak<AxVM>`，避免
  `AxVM -> device -> accessor -> AxVM` 强引用环。
- accessor 的 object/buffer read/write 全部调用 AxVM 受锁保护的复制 API；补齐空
  buffer、跨 page/area、未映射、地址溢出和 VM 已销毁错误映射。
- 审计 `axvirtio-common`，确保生产数据路径只使用 accessor 的复制方法，不在锁外
  解引用 `translate_and_get_limit` 返回的地址。若 trait 形状无法表达该约束，先把
  safe copy 设为必需 capability，再接入设备，不用 root walker 绕过所有权。
- 修正 `AddrSpace::translate_and_get_limit` 的 limit 为当前 GPA 到所在连续 area 末尾
  的剩余长度，并增加从 area 中间及末尾访问的回归测试。
- accessor 创建必须发生在 VM memory layout 建立之后；设备 factory 可以在 prepare
  阶段持有 accessor，但不能缓存某次 prepare 的 address-space 内部引用。

### 阶段 2：VM-local IRQ sink 与可重复 prepare

- 在 `axvm` 提供 VM-local `IrqSink`：持有 `Weak<AxVM>`、prepare generation 和明确的
  目标 vCPU policy；单 vCPU MVS 固定 vCPU 0，多 vCPU 支持前必须定义 affinity/路由，
  不能广播掩盖问题。
- `pulse` 调用 pending IRQ queue，并唤醒目标 vCPU；不直接访问 GIC system register。
  调用前校验 sink generation 仍是 VM 当前 generation；`IrqError` 保留 VM 不存在、
  generation 过期、非 Running/Paused、目标 vCPU 不存在等可诊断原因。
- virtio-mmio 首版按 edge pulse 使用；DTB trigger cell 与 `InterruptTriggerMode::Edge`
  保持一致。若以后改为 level，必须同时增加 interrupt ACK 后 deassert 事件，不能只把
  DTB flag 改成 level-high。
- 持久保存的是 per-VM prepare provider/profile，而不是某一代 `InterruptFabric` 或
  device 实例。reset/stopped-start 时由 profile 重建 factory registry、fabric 和新的
  device generation；默认 VM 仍使用内建 profile。
- 测试 IRQ 从非 vCPU host task 发出时只进入目标 pending queue，在 vCPU drain 时才
  调用架构注入；覆盖 stop/reset 后旧 sink 失败而非注入旧 VM。

### 阶段 3：AxVisor device adapter 与 factory

- 在 `os/axvisor/src/virtio_net/` 新增 `adapter.rs`、`factory.rs` 和 `config.rs`。
- adapter 拥有 `Arc<VirtioMmioNetDevice<...>>`、`IrqLine`、稳定的
  `Box<[Resource]>` 和只读 runtime endpoint；不把 IRQ endpoint 放进公开共享锁。
- `Device::handle` 只接受 MMIO，传递绝对 GPA 和同型 `AccessWidth`。read/write 错误
  映射成可诊断 `DeviceError`；写入返回 `InterruptPending` 时 pulse。transport 已在
  status=0 时完成 reset，adapter 不做重复 reset。
- factory 在 `build` 中严格解析配置、解析 `Edge` IRQ、构造 accessor/backend/device
  并返回 `DeviceBundle`。不使用回调和全局 slot。
- AxVisor 为每个 VM 创建独立 prepare profile；profile 每代构造
  `DeviceFactoryRegistry` 和 interrupt fabric，默认 factory 注册仍由
  `register_builtin_factories` 完成。
- host 测试通过 `AxVmDevices::build_with_factories` 走真实 MMIO router，覆盖 probe
  寄存器、完整 feature/queue setup、TX notify、interrupt status/ack、非法宽度、越界
  MMIO、坏 MAC 和缺失 IRQ backend。

### 阶段 4：从配置生成 virtio-mmio DTB 节点

- 在 AArch64 runtime FDT patch 流程中读取 `AxVMCrateConfig.devices.emu_devices`，只为
  `VirtioNet` 生成节点；不要把所有 emulated device 都伪装成 virtio-mmio。
- 生成 `virtio_mmio@<base>`，至少包含：
  `compatible = "virtio,mmio"`、按父节点 cell 宽度编码的 `reg`、
  `interrupts = <0 (irq_id - 32) 1>` 和 `interrupt-parent`。
- 查找实际交给 guest 的 GIC interrupt-controller，验证 `#interrupt-cells = <3>`；若
  无 phandle，分配一个不冲突的 phandle并写回控制器节点。存在多个候选控制器时必须
  按 guest GIC 配置明确选择，禁止取遍历到的第一个。
- 检测同名节点、重叠 `reg`、`irq_id < 32`、整数溢出和与 DTB 现有设备的冲突并返回
  错误，不覆盖已有节点。
- 用包含 64-bit root address cells、已有/缺失 phandle、多个 interrupt-controller 和
  冲突节点的 fixture 做 encode/decode 回归测试；再用 `dtc -I dtb -O dts` 检查产物。

### 阶段 5：确定性 UDP echo backend 与 RX worker

- `backend.rs` 实现有界队列和事件唤醒，队列满时返回明确 backpressure/drop 计数，
  禁止无限 `VecDeque<Vec<u8>>`。
- 首版虚拟 peer 只支持烟测所需的 Ethernet II、ARP、IPv4 和 UDP：所有长度、版本、
  ethertype、fragment/offload 条件先 checked validate，再构造 ARP/UDP 响应；正确交换
  MAC/IP/port并计算 IPv4/UDP checksum。其他帧显式 drop 并记原因。
- `transmit` 只入队并唤醒 worker，不重入设备。RX worker 等待事件而不是 sleep 轮询；
  `NoGuestBuffer` 使用有界重试/退避，超过预算明确 drop，不能忙等。
- AxVisor 在 VM prepare 成功且 `register_vm` 完成后，从该 VM device registry
  downcast adapter，取得 runtime endpoint 并启动 worker；以 `(VMId, generation)`
  管理 cancel token 和 task handle。stop/reset/remove/失败回滚时先取消并 join，再
  丢弃 device；reset 后只启动一个新 generation worker。
- host 测试覆盖 ARP、UDP echo、校验和、短帧/畸形长度、队列满、无 RX buffer 后重试、
  cancel/join，以及 TX backend 在 queue lock 内不会重入 device。

### 阶段 6：端到端 guest 验证

- 使用 `cargo xtask`/`scripts/axbuild` 的既有流程构建启用 virtio-net 前端和 UDP smoke
  app 的 ArceOS AArch64 镜像，不在计划中写死开发机绝对路径。
- guest 使用固定本机 IP，向虚拟 peer IP/port 发送带唯一 token 的 UDP payload，设置
  超时并校验源地址、长度和完整 payload。
- 分三步运行：无 net 的 emulated IRQ 启动基线；有设备的 probe/queue/IRQ；UDP echo。
- 保存关键日志：DTB 枚举、virtio feature/queue ready、TX/RX descriptor completion、
  pending IRQ 入队/在目标 vCPU drain，以及 guest `UDP_ECHO_PASS <token>`。
- 再执行一次 VM reset/restart，确认设备重新枚举、echo 再次成功且没有旧 worker 日志。

## 5. 最小可验证切片与退出条件

MVS 是阶段 0 到 6，不能把“阶段 0 到 4 可枚举”描述为网络集成完成。中间门槛为：

1. **内存门槛**：跨 area/page copy 测试通过，旧的 limit 高估测试先红后绿；
2. **IRQ 门槛**：非 vCPU task pulse 经 pending queue 到目标 vCPU，无直接 LR 写入；
3. **枚举门槛**：DTB round-trip 正确，guest 完成 `DRIVER_OK`；
4. **数据门槛**：host protocol tests 和 guest UDP echo 都通过；
5. **生命周期门槛**：reset 后第二次 echo 通过，旧 worker 已退出。

首版可暂缓多队列、mergeable RX buffer、control queue、TAP、真实交换机和多 vCPU IRQ
affinity；不能暂缓内存访问安全、IRQ 上下文正确性、worker 清理或错误传播。

## 6. 验证命令

代码阶段按实际修改的 crate 执行：

```bash
cargo fmt --all
cargo xtask clippy --package axaddrspace
cargo xtask clippy --package axvm
cargo xtask clippy --package axvisor
```

同时运行新增的最低层回归测试和现有 `axvirtio-common`、`axvirtio-net`、AxVM IRQ/FDT
测试。ArceOS/AxVisor 镜像构建和 QEMU 运行使用仓库对应的 `cargo xtask` 命令；只有在
xtask 无法表达专用配置且已检查其流程后，才使用匹配参数的原生 Cargo 命令。

## 7. 预计文件归属

- **axvm 通用能力**：guest memory accessor/copy 边界、VM-local queued IRQ sink、可重复
  prepare profile；不包含 loopback backend。
- **axvm AArch64 boot**：virtio-mmio DTB 节点生成与 GIC phandle/cell 校验。
- **AxVisor OS glue/runtime**：`os/axvisor/src/virtio_net/{mod,config,adapter,factory,
  backend,worker}.rs`，以及 VM prepare/stop/reset/remove 编排。
- **配置和 guest smoke app**：专用 AArch64 VM TOML、匹配 DTB/构建配置和 UDP echo
  测试入口。
- **保持不变的边界**：`axdevice` 不依赖 `axvirtio-net`；`axvirtio-net` 不依赖 AxVM、
  AxVisor、线程、GIC 或 FDT。
