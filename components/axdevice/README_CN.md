# axdevice

**axdevice** 是一个可复用、与操作系统无关的设备抽象层，专为虚拟机设计，支持在 `no_std` 环境中进行设备配置与 MMIO 模拟。适用于开发 hypervisor 或嵌入式操作系统。

## ✨ 特性亮点

- 📦 **模块化设计**：适用于任意操作系统或虚拟化平台的组件库。
- 🧩 **灵活设备抽象**：通过配置动态加载和注册设备。
- 🛠️ **无标准库依赖**：适配裸机、EL2 等场景，仅依赖 `core` 与 `alloc`。
- 🧵 **线程安全**：所有设备均用 `Arc` 管理，支持多核并发。
- 🧱 **便于扩展**：接入自定义设备只需实现 `BaseDeviceOps` trait。

## 📦 模块结构

- `config.rs`: 定义 `AxVmDeviceConfig`，用于初始化设备配置。
- `device.rs`: 定义 `AxVmDevices`，管理设备并处理 MMIO 读写。

## 📐 依赖图

```text
               +-------------------+
               |  axvmconfig       | <- 提供 EmulatedDeviceConfig
               +-------------------+
                         |
                         v
+------------------+     uses      +-----------------------+
|  axdevice        +-------------->+  axdevice_base::trait |
|  (当前模块)      |               +-----------------------+
+------------------+                      ^
        |                                 |
        v                                 |
+------------------+                      |
|  axaddrspace     | -- GuestPhysAddr ----+
+------------------+
```

## 🔁 使用流程

```text
[1] 加载设备配置 Vec<EmulatedDeviceConfig>
        ↓
[2] 构造 AxVmDeviceConfig
        ↓
[3] AxVmDevices::new() 初始化所有设备
        ↓
[4] guest发起 MMIO 访问
        ↓
[5] 匹配设备地址范围
        ↓
[6] 调用设备 trait 接口 handle_read / handle_write
```

## 🚀 示例代码

```rust
use axdevice::{AxVmDeviceConfig, AxVmDevices};

let config = AxVmDeviceConfig::new(vec![/* EmulatedDeviceConfig */]);

let devices = AxVmDevices::new(config);

let _ = devices.handle_mmio_read(0x1000_0000, 4);
devices.handle_mmio_write(0x1000_0000, 4, 0xdead_beef);
```

## 🔧 依赖组件

- [`axvmconfig`](https://github.com/arceos-hypervisor/axvmconfig.git)
- [`axaddrspace`](https://github.com/arceos-hypervisor/axaddrspace.git)
- [`axdevice_base`](https://github.com/arceos-hypervisor/axdevice_crates.git)

其他依赖：

- `log`
- `alloc`
- `cfg-if`
- `axerrno`

## License

Axdevice 采用 Apache License 2.0 开源协议。详见 [LICENSE](./LICENSE) 文件。
