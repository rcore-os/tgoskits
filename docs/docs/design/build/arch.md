---
sidebar_position: 3
sidebar_label: "Arch / Target 映射"
---

# Arch / Target 映射

当前仓库同时存在 `--arch` 与 `--target` 两套参数表达。

## 使用习惯

- **用户面**：经常使用 `--arch`（如 `riscv64`, `aarch64`）
- **构建系统**：解析为具体 target triple（如 `riscv64gc-unknown-none-elf`）
- 不同子系统的默认值不完全相同

## 高频映射

| 场景 | 推荐写法 |
|------|---------|
| ArceOS 最小路径 | `--arch riscv64` |
| StarryOS 最小路径 | `--arch riscv64` |
| Axvisor 最小路径 | `--arch aarch64` |

## 实践建议

1. 日常使用优先按文档示例写 `--arch`
2. 调试工具链、链接或平台问题时，查看 target triple
3. 混用旧命令时，先确认当前子命令接受的是 `--arch` 还是 `--target`

完整映射表：[构建系统完整说明](/docs/design/reference/build-system)
