# axvmconfig

> 路径：`virtualization/axvmconfig`
> 分层：客户机用户配置模型

`axvmconfig` 定义 Axvisor 面向用户的 TOML 语义。它表达客户机身份、CPU、镜像、内存、物理设备选择和开放式虚拟设备请求，但不分配地址或中断，也不依赖具体设备实现。

## 根结构

`GuestConfig` 包含：

- `base: VMBaseConfig`
- `kernel: VMKernelConfig`
- `devices: GuestDevices`

用户可见结构使用 `deny_unknown_fields`。格式不提供旧字段 alias。

## 虚拟设备请求

```toml
[[devices.virtual]]
id = "data0"
model = "virtio-blk-mmio"
capacity = "20GiB"
backend = { type = "file", path = "/images/data.raw" }
```

解析结果为 `VirtualDeviceRequest { id, model, options }`：

- `id` 是 VM 内唯一、稳定的实例 ID；
- `model` 是代码 catalog 使用的规范名称；
- 其余字段原样收集到 `toml::Table`，由具体 factory 通过带 `deny_unknown_fields` 的类型化配置解释。

`axvmconfig` 不知道 `virtio-blk-mmio` 的字段，也不维护设备类型 enum。未知 model 在 AxVM catalog 实例化阶段失败。当前 PR 未实现真正的 virtio-blk，因此上例不会伪装为可用磁盘。

配置禁止 MMIO、PIO、IRQ、MSI、LPI 等数字资源。固定资源只来自架构 machine profile 或规范化 host 固件。

## 物理设备选择

```toml
[devices]
passthrough = [{ path = "/soc/ethernet@1000" }]
disabled = [{ path = "/soc/gpio@2000" }]
```

`PhysicalDeviceRef` 使用绝对固件路径，不携带裸地址、长度或 IRQ。平台发现层把它解析为 host snapshot 和 typed assignment。`virtualized` 客户机从空地址空间开始；`passthrough` 客户机从 guest-assignable host 空间开始，再由最终设备图扣除禁用设备、客户机 RAM、虚拟设备和 host replacement。

## 镜像与内存

`VMKernelConfig` 保留内核、固件、DTB、ramdisk、启动协议、命令行和 `memory_regions`。未接入设备模型的 `kernel.disk_path` 已删除；未来虚拟磁盘必须通过 `devices.virtual` 表达。

## 已删除格式

以下字段会作为未知字段拒绝：

- `version`、`vm_type`、`address_space_policy`、`interrupt_mode`；
- `emu_devices`、数字设备类型、`cfg_list`、`irq_id`；
- `kernel.disk_path`；
- 裸 passthrough 地址/IRQ、`passthrough_addresses`、`passthrough_ports`；
- 用户覆盖 machine 串口、中断控制器或固件固定资源的字段。

这是破坏性迁移，不保留 deprecated alias 或双路径。

## 工具与验证

宿主 `std` 构建提供 JSON Schema、`axvmconfig check` 与配置生成工具。验证至少包括：

```bash
cargo test -p axvmconfig
cargo run -p axvmconfig -- check --config-path <guest.toml>
cargo xtask axvisor config vm --help
```

仓库配置应全部能由当前格式解析；`emu_devices`、`irq_id` 与 `kernel.disk_path` 必须有明确拒绝测试。

新增设备教程见 [Axvisor 虚拟设备模型与新增设备教程](/docs/architecture/axvisor/emulated-devices)。
