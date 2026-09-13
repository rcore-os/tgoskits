# Axvisor virtio-net 设备设计

## 问题与成功标准

Axvisor 必须能够启动两个 ArceOS 客户机，为每个客户机提供一个 VirtIO MMIO
网络设备，并在二者之间转发以太网帧。完成标准是两个客户机都能发现设备，
并通过确定性的双向网络测试；测试期间不得发生 MMIO fault、队列停滞或描述符泄漏。

初始范围只包含进程内二层交换机。物理宿主网卡上联、多队列 VirtIO、卸载、
在线迁移和直通均不在本设计范围内。

## 现有代码与未采用的复用方案

`debin/virtio-net-2` 包含可复用的 `axvirtio-common`、`axvirtio-net` crate
以及一份旧 Axvisor 适配器。网络相关 crate 及其测试予以保留；`axvirtio-blk`
延后到有真实块设备消费者的独立改动。旧适配器不能原样复制，因为当前
`axdevice` 只允许设备通过作用域受限的 `DeviceContext` 获得客户机内存 DMA
能力。旧适配器在后台 worker 中长期持有 VM 级客户机内存访问器，绕过了这条
能力边界。

## 选定架构

实现明确分为四层：

1. `axvirtio-common` 持有 VirtIO MMIO 状态与 split-ring 机制。队列布局长期
   保存，但每次读写客户机内存的操作都必须取得作用域受限的内存能力。
2. `axvirtio-net` 持有 RX/TX 描述符校验和设备状态机，不依赖 Axvisor 或
   ArceOS。
3. `axdevice` 提供与具体 bundle 设备和 `DmaGrant` 绑定的 DMA pollable
   能力。runtime 每次轮询都重新构造作用域受限的 `DeviceContext`，设备能力
   不能保存该上下文。
4. Axvisor 注册 `virtio-net` `DeviceModel`。每个实例持有一个交换机端口、
   MMIO 分配、wired IRQ 和 DMA grant。设备轮询将有界 ingress 帧写入客户机
   RX ring，且只在发布 used descriptor 后触发 IRQ。

```text
客户机 TX kick
  -> Device::write(DeviceAccess, 作用域受限的 DeviceContext)
  -> 校验并读取 TX 描述符链
  -> 交换机分类帧
  -> 目标端口的有界 ingress 队列

vCPU0 设备轮询
  -> DMA poll 能力（作用域受限的 DMA）
  -> 写入目标 RX 描述符链与 used ring
  -> 发布中断状态
  -> 触发目标 wired IRQ
```

## 所有权与同步

- `DeviceModel` 持有每个网卡的不可变配置。
- 构建后的设备持有 VirtIO 状态及其 DMA grant。
- 每个交换机 attachment 都有一个携带 generation 的唯一端口身份；其 RAII
  注册在销毁时移除端口。
- TX 处理在设备队列锁内执行。交换机 backend 只将帧副本放入队列，绝不回调
  VirtIO 设备。
- RX ingress 队列有容量上限。端口队列已满时只丢弃发往该端口的副本。
- 设备轮询负责推进 RX。IRQ 注入发生在客户机内存写入完成之后，并尽量避开
  队列状态修改区间。
- 作用域受限的 `DeviceContext` 引用不会被保存、共享或移动到任务中，从而
  维持 VM 生命周期和 DMA 授权边界。

## 失败语义

无效描述符、不可访问的客户机内存、不支持的功能和资源规划失败均返回类型化
错误。队列耗尽和缺少 RX buffer 是可观察的流控结果，不能伪造成成功。任何路径
都不得猜测默认 MMIO 地址、IRQ、MAC 地址或宿主接口。

## 备选方案

| 方案 | 结论 |
| --- | --- |
| 在 worker 中保存弱引用 `AxVM` 客户机内存访问器 | 不采用：它绕过按访问作用域授予的 DMA grant，并将设备耦合到 AxVM。 |
| 只在客户机 MMIO kick 时投递 RX | 不采用：最后一次 RX kick 后到达的帧可能永久停滞。 |
| 增加通用 DMA pollable 能力 | 采用：在不保存 VM 内存访问器的前提下保持显式授权并支持异步设备推进。 |
| 从物理上联 worker 起步 | 延后：证明双客户机 VirtIO 网络不需要该能力，而且会引入宿主驱动所有权风险。 |

## 验证

- 现有 split-ring、MMIO、block、net 和 switch 测试必须继续通过。
- 增加能力测试，证明未注册或不匹配的 DMA grant 会被拒绝，且正确设备在轮询
  期间能获得作用域受限的内存能力。
- 增加 Axvisor model 测试，覆盖 options、资源需求和 bundle grant。
- 在 QEMU/Axvisor 下启动两个 ArceOS 客户机，并要求确定性的双向数据包交换。
  从干净 checkout 开始时，应先构建每个镜像再启动 Axvisor（VM TOML 文件有意
  引用这些生成文件）：

  ```bash
  apps/arceos/virtio-net-peer/run.sh
  ```

  工具链安装在固定 Rust sysroot 之外时，需要设置 `LLVM_OBJCOPY`。

  QEMU runner 必须同时看到 `VM1_VIRTIO_NET_PASS` 和 `VM2_VIRTIO_NET_PASS`；
  任一 `*_FAIL` 标记或 panic 都表示失败。
- 对每个改动的 crate 运行 `cargo fmt` 和定向 clippy。
