# TGOSKits

[![Build & Test](https://github.com/rcore-os/tgoskits/actions/workflows/test.yml/badge.svg)](https://github.com/rcore-os/tgoskits/actions/workflows/test.yml)

## 简介

TGOSKits 是一个面向操作系统开发的组件集成仓库，通过 Git Subtree 技术将 60+ 个独立的组件仓库（包括 ArceOS、Axvisor、StarryOS 等操作系统及其组件）整合到统一的主仓库中。

- **统一管理** - 单一仓库集中管理所有已有操作系统组件，并完整保留每个组件的独立开发历史
- **双向同步** - 支持主仓库与组件仓库之间的代码同步，可选在独立组件仓库开发也可以使用 TGOSKits 仓库统一开发
- **完整测试** - 即提供了在 TGOSKits 仓库的系统及单元测试，也支持独立组件仓库的单元测试和集成测试
- **版本控制** - 统一的版本发布处理，以便将众多组件统一发布到 crates.io

## 组件管理

TGOSKits 通过 Git Subtree 技术管理 60+ 个独立仓库的组件，使用 [scripts/repo/repos.csv](scripts/repo/repos.csv) 记录组件的来源 URL、目标路径、分支等信息。`scripts/repo/repo.py` 是基于此配置的维护工具。

```bash
# 列出组件
python3 scripts/repo/repo.py list

# 添加/移除组件
python3 scripts/repo/repo.py add --url <url> --target <dir>
python3 scripts/repo/repo.py remove <name> --remove-dir

# 切换分支
python3 scripts/repo/repo.py branch <name> <branch>

# 双向同步（一般由 CI 自动完成）
python3 scripts/repo/repo.py pull <name>   # 从组件仓库拉取
python3 scripts/repo/repo.py push <name>   # 推送到组件仓库
```

详细说明见 [docs/repo.md](docs/repo.md)。

## 快速开始

### 环境要求

- Ubuntu 22.04 及以上或者类似 Linux 系统

- Rust 1.75+ / Python 3.6+ / Git 2.0+

- `cargo install ostool --version ^0.8`

### 开发流程

TGOSKits 仓库统一管理所有组件，便于开发及测试，使用 VSCode 搭配 [rust-analyzer](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust-analyzer) 和 [Rust Targets](https://marketplace.visualstudio.com/items?itemName=PolyMeilex.rust-targets) 这两个插件可以体验完整的 IDE 开发过程。

```bash
# 1. 克隆仓库
git clone https://github.com/rcore-os/tgoskits.git
cd tgoskits

# 2. 创建并切换到自己的分支
git branch xxx

# 3. 修改组件代码
vim components/arm_vcpu/src/lib.rs

# 4. 提交
git add *
git commit -m "feat(arm_vcpu): add new feature"

# 5. 推送到主仓库
git push origin main
```

本仓库中配置了 CI 会在代码推送到主仓库后自动将修改同步到对应的独立组件仓库的 `mirror` 分支，同样也会从组件仓库拉取更新到主仓库，只需要在当前仓库编辑代码后直接提交到当前仓库即可，一般无需手动处理同步问题！

> 完整开发过程，详细说明见 [docs/repo.md](docs/repo.md)。

### 构建和测试

当前仓库提供了本地开发测试以及 CI 自动测试支持，在本地开发时，直接运行 `cargo xtask test xxx_os --target xxx` 即可快速测试

## 贡献

1. Fork 仓库并创建分支
2. 进行修改（遵循 Rust 代码规范，添加必要测试）
3. 提交更改（使用清晰的提交信息）
4. 推送分支并创建 Pull Request

发现 bug 或有功能建议请创建 Issue。

## 许可证

采用 `Apache-2.0` 许可协议，各组件可能有其独立的许可证，详见各组件目录下的 LICENSE 文件。
