---
sidebar_position: 3
sidebar_label: "TOML 字段参考"
---

# TOML 字段参考

改 Axvisor 配置时需要重点关注的字段。

## 板级配置常看项

| 字段 | 说明 |
|------|------|
| `target` | 构建目标 triple |
| `features` | 编译 feature 列表 |
| `log` | 日志级别 |
| `vm_configs` | 默认 VM 配置列表 |

## VM 配置常看项

| 字段 | 说明 |
|------|------|
| `id`, `name` | VM 标识 |
| `cpu_num`, `phys_cpu_ids` | CPU 配置 |
| `entry_point` | 入口地址 |
| `kernel_path`, `kernel_load_addr` | 内核路径与加载地址 |
| `memory_regions` | 内存区域定义 |
| 设备与中断字段 | 设备映射 |

## 排查顺序

1. 确认配置文件是否被当前路径加载
2. 确认镜像路径和文件系统布局是否匹配
3. 检查内存、中断和设备映射细节

字段详解：[AxVisor 内部机制](/docs/design/architecture/axvisor-internals)
