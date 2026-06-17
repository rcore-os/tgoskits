# Starry Nginx App

Starry 下 nginx 应用的构建与测试入口。

## 测试模式

guest 内统一入口 `/usr/bin/nginx-runner.sh <mode>`：

| mode | 用途 | CI |
|------|------|----|
| `smoke` | 仅 smoke | ✅ 唯一接入上层 CI |
| `phase <id>` | 单阶段复测 | ❌ 手工 |
| `all` | smoke + 全部 phase（阶段强隔离） | ❌ 手工 |
| `stress` | 压测（当前 skip） | ❌ 手工 |
| `debug <name>` | 单问题调试 | ❌ 手工 |

QEMU 入口按用途分目录：根 `qemu-<arch>.toml`（CI）、`qemu/all/`（手工全量）、
`qemu/phase/`（手工单阶段）、`qemu/debug/`（手工调试）。

## 用法

```bash
# CI smoke（四架构）
cargo xtask starry app qemu -t nginx --arch x86_64 \
  --qemu-config apps/starry/nginx/qemu/smoke/qemu-x86_64.toml

# 手工单阶段
cargo xtask starry app qemu -t nginx --arch x86_64 \
  --qemu-config apps/starry/nginx/qemu/phase/qemu-x86_64-phase31.toml
```

## 详细设计

完整设计（目录结构、统一入口 marker 规则、all 模式阶段隔离契约、TOML 整合规则、
迁移步骤）见 [`www/nginx-ci-refactor-proposal.md`](../../../www/nginx-ci-refactor-proposal.md)。
