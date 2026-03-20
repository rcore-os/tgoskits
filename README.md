# TGOSKits

[![Build & Test](https://github.com/rcore-os/tgoskits/actions/workflows/test.yml/badge.svg)](https://github.com/rcore-os/tgoskits/actions/workflows/test.yml)

**一站式操作系统开发组件集成仓库**

TGOSKits 是一个面向操作系统开发的组件集成仓库，通过 Git Subtree 技术将 60+ 个独立的组件仓库整合到统一的主仓库中。

## ✨ 核心特性

- **🎯 统一管理** - 单一仓库集中管理所有已有操作系统组件，并完整保留每个组件的独立开发历史
- **🔄 双向同步** - 支持主仓库与组件仓库之间的代码同步，可选在独立组件仓库开发也可以使用 TGOSKits 仓库统一开发
- **✅ 完整测试** - 即提供了在 TGOSKits 仓库的系统及单元测试，也支持独立组件仓库的单元测试和集成测试
- **📦 版本控制** - 统一的版本发布处理，以便将众多组件统一发布到 crates.io

## 📚 快速导航

### 🚀 快速上手
- [快速开始指南](docs/quick-start.md) - 5分钟快速上手 TGOSKits
- [环境配置](docs/quick-start.md#环境配置) - 详细的环境配置说明
- [构建系统](docs/build-system.md) - 构建系统详解

### 🖥️ 操作系统开发指南
- [ArceOS 开发指南](docs/arceos-guide.md) - 模块化操作系统开发
- [StarryOS 开发指南](docs/starryos-guide.md) - 教学操作系统开发  
- [Axvisor 开发指南](docs/axvisor-guide.md) - 虚拟机监控器开发

### 🧩 组件开发
- [组件开发指南](docs/components.md) - 如何开发和维护组件
- [组件列表](scripts/repo/repos.csv) - 60+ 可用组件清单
- [仓库管理](docs/repo.md) - Git Subtree 管理详解

## 🏗️ 项目架构

```
tgoskits/
├── os/                      # 操作系统项目
│   ├── arceos/             # ArceOS - 模块化操作系统/Unikernel
│   ├── axvisor/            # Axvisor - Type I 虚拟机监控器
│   └── StarryOS/           # StarryOS - 教学操作系统
│
├── components/              # 60+ 可复用组件库
│   ├── Hypervisor/         # 虚拟化组件（arm_vcpu, axvm, axvisor_api 等）
│   ├── ArceOS/             # ArceOS 框架组件（axcpu, axsched, axdriver 等）
│   ├── Starry/             # StarryOS 组件（starry-process, axpoll 等）
│   └── rCore/              # rCore 生态组件
│
├── scripts/                 # 构建和管理脚本
│   ├── repo/               # Git Subtree 管理工具
│   └── test/               # 测试脚本
│
├── docs/                    # 项目文档
├── xtask/                   # Rust 构建任务工具
├── test-suit/              # 测试套件
└── platform/               # 平台支持包
```

### 📊 组件统计

| 分类 | 数量 | 说明 | 代表组件 |
|------|------|------|----------|
| **Hypervisor** | 20 | 虚拟化支持 | `arm_vcpu`, `axvm`, `axvisor_api`, `riscv_vcpu` |
| **ArceOS** | 24 | OS 框架核心 | `axcpu`, `axsched`, `axerrno`, `axdriver_crates` |
| **OS** | 3 | 完整操作系统 | `ArceOS`, `Axvisor`, `StarryOS` |
| **Starry** | 9 | StarryOS 专用 | `starry-process`, `starry-signal`, `axpoll` |

## 🛠️ 快速开始

### 环境要求

- **操作系统**: Ubuntu 22.04+ 或类似 Linux 系统
- **Rust**: 1.75+ 
- **Python**: 3.6+
- **Git**: 2.0+
- **工具**: `cargo install ostool --version ^0.8`

### 一键构建和运行

```bash
# 1. 克隆仓库
git clone https://github.com/rcore-os/tgoskits.git
cd tgoskits

# 2. ArceOS 示例 - Hello World
cargo xtask arceos run --package arceos-helloworld --arch riscv64

# 3. StarryOS
cargo xtask starry rootfs --arch riscv64
cargo xtask starry run --arch riscv64 --package starryos

# 4. Axvisor (需要先准备 Guest 镜像)
cd os/axvisor
cargo xtask defconfig qemu-aarch64
cargo xtask build
```

> 详细说明请查看 [快速开始指南](docs/quick-start.md)

## 📦 组件管理

TGOSKits 通过 Git Subtree 技术管理 60+ 个独立仓库的组件，使用 [scripts/repo/repos.csv](scripts/repo/repos.csv) 记录组件的来源 URL、目标路径、分支等信息。

### 常用命令

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

> 详细说明见 [仓库管理指南](docs/repo.md)。

## 👨‍💻 开发流程

TGOSKits 仓库统一管理所有组件，便于开发及测试。

### IDE 配置

推荐使用 VSCode 搭配以下插件：
- [rust-analyzer](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust-analyzer) - Rust 语言支持
- [Rust Targets](https://marketplace.visualstudio.com/items?itemName=PolyMeilex.rust-targets) - 多目标支持

### 基本流程

```bash
# 1. 克隆仓库
git clone https://github.com/rcore-os/tgoskits.git
cd tgoskits

# 2. 创建并切换分支
git checkout -b my-feature

# 3. 修改组件代码
vim components/arm_vcpu/src/lib.rs

# 4. 提交更改
git add .
git commit -m "feat(arm_vcpu): add new feature"

# 5. 推送到主仓库
git push origin my-feature
```

> 💡 **提示**: CI 会自动将修改同步到对应的独立组件仓库的 `mirror` 分支

## 🧪 测试

```bash
# 测试 ArceOS
cargo xtask test arceos --target riscv64gc-unknown-none-elf

# 测试 StarryOS  
cargo xtask test starry --target riscv64gc-unknown-none-elf

# 测试 Axvisor
cargo xtask test axvisor --target aarch64-unknown-none-softfloat
```

## 🤝 贡献

我们欢迎所有形式的贡献！

1. Fork 仓库并创建分支
2. 进行修改（遵循 Rust 代码规范，添加必要测试）
3. 提交更改（使用清晰的提交信息）
4. 推送分支并创建 Pull Request

发现 bug 或有功能建议请创建 Issue。

## 📄 许可证

采用 `Apache-2.0` 许可协议，各组件可能有其独立的许可证，详见各组件目录下的 LICENSE 文件。

## 🔗 相关链接

- [ArceOS 官方文档](https://arceos-org.github.io/arceos/)
- [Axvisor 官方文档](https://arceos-hypervisor.github.io/axvisorbook/)
- [rCore Tutorial](https://rcore-os.cn/rCore-Tutorial-Book-v3/)
- [Rust OSDev 社区](https://rust-osdev.com/)

---

**Happy Coding! 🎉**
