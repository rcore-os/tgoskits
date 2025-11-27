# AxVisor 设备树配置使用说明

本文档详细说明了在 AxVisor 中如何配置和使用设备树（FDT）来生成客户机 VM 的设备树。

本文档所述功能只在aarch64 架构下支持。

## 1. 概述

AxVisor 支持两种方式生成客户机 VM 的设备树：

1. **使用预定义的设备树文件**：通过 [kernel] 部分的 `dtb_path` 指定设备树文件路径
2. **动态生成设备树**：当 `dtb_path` 字段未使用时，根据配置文件中的参数动态生成设备树

无论采用哪种方式，CPU 节点和内存节点都会根据配置进行更新。

## 2. 配置文件结构

配置文件采用 TOML 格式，主要包含以下几个部分：

```toml
[base]
# 基本配置信息

[kernel]
# 内核和设备树配置

[devices]
# 设备配置信息
```

## 3. 设备树处理机制

### 3.1 使用预定义设备树文件

当 [kernel] 部分的 `dtb_path` 配置了设备树文件路径时：

```toml
[kernel]
dtb_path = "/path/to/device-tree.dtb"
```

AxVisor 会优先使用提供的设备树文件，并根据以下配置更新其中的 CPU 节点和内存节点：

- CPU 节点根据 [base] 部分的 `phys_cpu_ids` 更新
- 内存节点根据 [kernel] 部分的 `memory_regions` 更新

注意：当使用预定义设备树文件时，[devices] 部分的 `passthrough_devices` 中如果有规范化的[Name,Base-Ipa,Base-Pa,Length,Alloc-Irq]设备配置，则axvisor会直接按照 `passthrough_devices`中的配置给guest映射设备内存，设备树中的解析则会被忽略，只是将更改过内存和cpu的预定义设备树文件直接传给guest。

### 3.2 动态生成设备树

当 [kernel] 部分的 `dtb_path` 未添加时：

```toml
[kernel]
# dtb_path = ""
```

AxVisor 会根据配置文件中的参数动态生成客户机设备树：

1. **CPU 节点**：根据 [base] 部分的 `phys_cpu_ids` 生成
2. **内存节点**：根据 [kernel] 部分的 `memory_regions` 生成
3. **其他设备节点**：根据 [devices] 部分的 `passthrough_devices` 和 `excluded_devices` 生成

## 4. 配置参数详解

### 4.1 基本配置 [base]

```toml
[base]
id = 1                      # 客户机 VM ID
name = "linux-qemu"         # 客户机 VM 名称
vm_type = 1                 # 虚拟化类型
cpu_num = 1                 # 虚拟 CPU 数量
phys_cpu_ids = [0]          # 客户机 VM 物理 CPU 集合
```

注意：配置文件中的 `phys_cpu_sets` 字段已不再需要手动配置。AxVisor 会根据主机设备树和 `phys_cpu_ids` 自动识别并生成相应的 CPU 集合掩码。

### 4.2 内核配置 [kernel]

```toml
[kernel]
entry_point = 0x8020_0000           # 内核镜像入口点
image_location = "memory"           # 镜像位置 ("memory" | "fs")
kernel_path = "tmp/Image"           # 内核镜像文件路径
kernel_load_addr = 0x8020_0000      # 内核镜像加载地址
dtb_path = "tmp/linux.dtb"          # 设备树文件路径（空字符串表示动态生成）
dtb_load_addr = 0x8000_0000         # 设备树加载地址

# 内存区域配置，格式为 (基地址, 大小, 标志, 映射类型)
其中映射类型0为MAP_Alloc(由host负责，随机分配内存)，1为Map_Identical(由host负责1:1给guest映射内存，但是起始地址随机)，2为MAP_Reserved(由host负责，将host中一块标记为reserved的内存完全1:1映射给guest，起始地址和配置一致)
memory_regions = [
  [0x8000_0000, 0x1000_0000, 0x7, 0], # 系统 RAM 1G MAP_IDENTICAL
]
```

### 4.3 设备配置 [devices]

```toml
[devices]
# 直通设备配置（仅在动态生成设备树时生效）
passthrough_devices = [
  ["/intc"],
]

# 排除设备配置（仅在动态生成设备树时生效）
excluded_devices = [
  ["/intc"],
]
```

注意：直通设备配置已简化，现在只需要提供从根节点开始的完整路径即可，如 ["/intc"]。设备的地址、大小等信息会根据设备树自动识别并直通，无需手动填写。

## 5. 设备直通机制

### 5.1 直通设备配置

`passthrough_devices` 定义了需要直通给客户机的设备节点：

```toml
passthrough_devices = [
  ["/"],              # 直通根节点及其所有子节点
  ["/intc"],          # 直通 /intc 节点及其子节点
]
```

设备节点格式为从根节点开始的全局路径（如 `/intc`），在直通时会将以下节点包含在客户机设备树中：

1. 指定的直通节点本身
2. 直通节点的所有后代节点
3. 与直通设备相关的依赖节点

注意：
1. 此配置仅在动态生成设备树时生效，当使用预定义设备树文件时将被忽略。
2. 直通设备配置已简化，现在只需要提供从根节点开始的完整路径即可，设备的地址、大小等信息会根据设备树自动识别并直通。

### 5.2 排除设备配置

`excluded_devices` 定义了不希望直通给客户机的设备节点：

```toml
excluded_devices = [
  ["/timer"],         # 排除 /timer 节点及其子节点
]
```

在查找所有直通节点后，会将排除的节点及其后代节点从最终的客户机设备树中移除。

注意：此配置仅在动态生成设备树时生效，当使用预定义设备树文件时将被忽略。

### 5.3 直通地址配置

`passthrough_addresses` 定义了直通给客户机使用的地址信息：

```
passthrough_addresses = [
  [0x28041000, 0x100_0000],
]
```

该字段定义的地址会直通给客户机使用，这在某些情况下非常有用，例如设备树文件为非标准设备树格式或客户机系统时定制linux。

## 6. 示例配置

### 6.1 使用预定义设备树文件的配置

```toml
[base]
id = 1
name = "linux-qemu"
vm_type = 1
cpu_num = 2
phys_cpu_ids = [0, 1]
# phys_cpu_sets 不再需要手动配置，会自动根据 phys_cpu_ids 生成

[kernel]
entry_point = 0x8020_0000
image_location = "memory"
kernel_path = "tmp/Image"
kernel_load_addr = 0x8020_0000
dtb_path = "/home/user/device-tree.dtb"  # 使用预定义设备树文件
dtb_load_addr = 0x8000_0000

memory_regions = [
  [0x8000_0000, 0x1000_0000, 0x7, 1], # System RAM 1G MAP_IDENTICAL
]

[devices]
# 注意：以下配置在使用预定义设备树时将被忽略
passthrough_devices = [
  ["/intc"],
]
# 直通地址配置
passthrough_addresses = [
  [0x28041000, 0x100_0000],
]

excluded_devices = [
  ["/timer"],
]
```

### 6.2 动态生成设备树的配置

```toml
[base]
id = 1
name = "linux-qemu"
vm_type = 1
cpu_num = 2
phys_cpu_ids = [0, 1]
# phys_cpu_sets 不再需要手动配置，会自动根据 phys_cpu_ids 生成

[kernel]
entry_point = 0x8020_0000
image_location = "memory"
kernel_path = "tmp/Image"
kernel_load_addr = 0x8020_0000
# dtb_path = ""  # 不使用该字段表示动态生成设备树
dtb_load_addr = 0x8000_0000

memory_regions = [
  [0x8000_0000, 0x1000_0000, 0x7, 1], # System RAM 1G MAP_IDENTICAL
]

[devices]
# 以下配置仅在动态生成设备树时生效
# 注意：直通设备配置已简化，现在只需要提供从根节点开始的完整路径即可
passthrough_devices = [
  ["/"],
  ["/intc"],
]
# 直通地址配置
passthrough_addresses = [
  [0x28041000, 0x100_0000],
]
excluded_devices = [
  ["/timer"],
  ["/watchdog"],
]
```

## 7. 处理流程

1. **检查 dtb_path**：
   - 如果使用 `dtb_path` 字段，则加载并使用预定义的设备树文件，此时 `passthrough_devices` 和 `excluded_devices` 配置将被忽略
   - 如果未使用 `dtb_path` 字段，则动态生成设备树，此时 `passthrough_devices` 和 `excluded_devices` 配置生效

2. **CPU 节点处理**：
   - 根据 `phys_cpu_ids` 配置更新或生成 CPU 节点
   - 只包含配置中指定的 CPU
   - 自动根据 `phys_cpu_ids` 生成 `phys_cpu_sets`，无需手动配置

3. **内存节点处理**：
   - 根据 `memory_regions` 配置更新或生成内存节点
   - 按照指定的地址和大小创建内存区域

4. **设备节点处理**（仅在动态生成时）：
   - 根据 `passthrough_devices` 确定需要包含的设备节点
   - 包括直通节点、其后代节点以及相关依赖节点
   - 根据 `excluded_devices` 排除指定的设备节点及其后代节点

5. **生成最终设备树**：
   - 将处理后的节点组合成完整的设备树
   - 存储在全局缓存中供后续使用

## 8. 特别配置
1. **qemu 启动参数**：
```
  arceos_args = ["BUS=mmio", "BLK=y", "LOG=info", "SMP=4", "MEM=8g",
                "QEMU_ARGS=\"-machine gic-version=3  -cpu cortex-a72 -append 'root=/dev/vda rw init=/init' \"",
                "DISK_IMG=\"tmp/qemu/rootfs.img\"",]
```
其中当不提供设备树时 `-append 'root=/dev/vda rw init=/init'`参数必须添加，目的是在主机设备树中添加chosen节点的bootargs属性。