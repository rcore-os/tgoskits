---
sidebar_position: 3
sidebar_label: "贡献指南"
---

# 贡献指南

感谢您对 TGOSKits 的关注！我们欢迎各种形式的贡献，包括代码、文档、测试、设计、仓库治理和社区建设。本指南将帮助您了解如何参与当前工作区的协作。

## 贡献方式

### 报告问题
如果您希望通过反馈 bug 或问题参与贡献：

- 请优先阅读[获取帮助](/community/support)页面中的“问题反馈”部分，按照说明准备复现信息
- 然后在 [GitHub Issues](https://github.com/rcore-os/tgoskits/issues) 中创建 Issue

这样可以减少重复提问，也便于维护者更高效地定位和解决问题。

### 功能建议
如果您有新功能的想法并希望推动其实现：

- 优先在 [GitHub Issues](https://github.com/rcore-os/tgoskits/issues) 中说明使用场景、预期收益和现有文档背景
- 先与社区共同收敛需求，再根据共识选择是否发起实现工作

关于提问与一般使用问题的支持方式，请参考[获取帮助](/community/support)。

### 代码贡献
我们欢迎代码贡献！以下是贡献代码的流程：

#### 开发环境设置
1. Fork [TGOSKits 仓库](https://github.com/rcore-os/tgoskits)
2. 克隆您的 fork：
   ```bash
   git clone https://github.com/<your-github-name>/tgoskits.git
   cd tgoskits
   ```
3. 添加上游仓库：
   ```bash
   git remote add upstream https://github.com/rcore-os/tgoskits.git
   ```
4. 创建新的分支：
   ```bash
   git checkout -b feature/your-feature-name
   ```

#### 代码规范
- 遵循 [Rust 官方代码风格](https://rust-lang.github.io/api-guidelines/)
- 使用 `cargo fmt` 格式化代码
- 使用 `cargo clippy` 检查代码质量
- 提交前建议额外运行 `cargo xtask sync-lint`，检查可疑的原子同步内存序用法；如果被该检查阻止，可参考[同步内存序检查](/community/sync-lint)
- 对系统构建、运行和测试，优先使用 `cargo xtask` 提供的统一入口
- 为公共 API 编写文档注释
- 为新功能添加测试

#### 提交 Pull Request
1. 确保您的代码与主分支保持最新：
   ```bash
   git fetch upstream
   git rebase upstream/main
   ```
2. 运行测试确保所有测试通过：
   ```bash
   cargo xtask test
   cargo xtask clippy
   ```
3. 提交您的更改：
   ```bash
   git commit -m "feat: add your feature description"
   ```
4. 推送到您的 fork：
   ```bash
   git push origin feature/your-feature-name
   ```
5. 在 GitHub 上创建 Pull Request

### 文档贡献
良好的文档对项目至关重要：

- 修正错误或不清楚的文档
- 添加使用示例和教程
- 翻译文档到其他语言
- 改进文档结构和导航

### 测试贡献
测试是确保代码质量的关键：

- 编写单元测试
- 添加集成测试
- 改进测试覆盖率
- 报告测试相关问题

## 开发指南

### 代码审查
所有代码贡献都需要经过代码审查：

- 维护者会审查您的 PR
- 您可能需要根据反馈进行修改
- 保持友好和专业的交流

### 发布流程
我们遵循语义化版本控制：

- 主版本号：不兼容的 API 修改
- 次版本号：向下兼容的功能性新增
- 修订号：向下兼容的问题修正

### 社区行为准则
我们致力于创建一个友好、安全和欢迎的环境：

- 尊重不同的观点和经验
- 使用友好和包容的语言
- 接受建设性批评
- 关注对社区最有利的事情
- 对其他社区成员表示同理心

## 获得认可

我们会在以下地方认可贡献者：

- [贡献者列表](/community/team)
- 发布说明中的致谢
- 项目 README 中的贡献者部分
- 特殊贡献的专门感谢

## 需要帮助？

如果您在贡献过程中需要帮助：

- 查看[支持页面](/community/support)获取帮助渠道
- 在社区讨论中提问
- 参考相关文档和示例

感谢您考虑为 TGOSKits 做出贡献！您的参与对整个工作区的演进都非常重要。
