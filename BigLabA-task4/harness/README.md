# StarryOS AI Harness

这个目录包含了为 StarryOS 构建的完整 AI 自动化测试与改进工具链。

## 📁 目录结构

```
harness/
├── README.md                          # 本文件
├── HARNESS_STATUS.md                  # 实现状态报告
├── test-harness.md                    # 测试报告
│
├── docs/                              # 文档目录
│   ├── AI_AUTOMATED_TESTING_SYSTEM.md # 完整系统设计文档 (2295 行)
│   ├── AI_SYSTEM_SUMMARY.md           # 快速参考指南
│   ├── INTERVIEW_AI_HARNESS.md        # 面试文档 (设计思路)
│   └── testing/                       # 测试数据
│       └── syscall-database.json      # 系统调用数据库
│
├── mcp-servers/                       # MCP Server 实现
│   └── starry-testing/
│       ├── index.js                   # 主服务器代码 (1088 行)
│       ├── package.json               # NPM 配置
│       ├── README.md                  # MCP Server 文档
│       ├── claude-config-example.json # Claude Code 配置示例
│       └── node_modules/              # 依赖包
│
└── test-cases/                        # 测试用例目录
    └── syscall/                       # 系统调用测试用例（待生成）
```

## 🚀 快速开始

### 1. 安装依赖

```bash
cd harness/mcp-servers/starry-testing
npm install
```

### 2. 配置 Claude Code

编辑 `~/.config/claude/settings.json`，添加：

```json
{
  "mcpServers": {
    "starry-testing": {
      "command": "node",
      "args": ["/path/to/StarryOS/harness/mcp-servers/starry-testing/index.js"],
      "env": {
        "STARRY_ROOT": "/path/to/StarryOS"
      }
    }
  }
}
```

### 3. 开始使用

在 Claude Code 中使用以下工具：
- `list_syscalls` - 列出系统调用
- `generate_test_case` - 生成测试用例
- `analyze_syscall` - 分析代码查找 Bug
- `get_test_coverage` - 获取测试覆盖率
- `suggest_next_target` - 获取智能推荐

## 📚 文档说明

### 核心文档

1. **AI_AUTOMATED_TESTING_SYSTEM.md** (2295 行)
   - 完整的系统设计文档
   - 包含架构、实现、使用指南
   - 适合深入了解整个系统

2. **AI_SYSTEM_SUMMARY.md**
   - 快速参考指南
   - 包含使用示例和常见问题
   - 适合快速上手

3. **INTERVIEW_AI_HARNESS.md** (642 行)
   - 面试级别的设计文档
   - 详细阐述设计动机和思路
   - 适合展示项目价值

### 状态报告

1. **HARNESS_STATUS.md**
   - 当前实现状态
   - 统计数据和进度
   - 下一步计划

2. **test-harness.md**
   - 测试验证报告
   - 功能检查清单
   - 已知问题

## 🔧 MCP Server

### 功能特性

- **7 个工具接口**：完整的系统调用测试和分析功能
- **数据库管理**：持久化存储测试结果和 Bug 信息
- **智能推荐**：基于优先级的任务推荐
- **测试生成**：自动生成 C 语言测试用例
- **Bug 分析**：静态代码分析，发现潜在问题

### 技术栈

- **运行时**: Node.js
- **协议**: MCP (Model Context Protocol)
- **SDK**: @modelcontextprotocol/sdk
- **语言**: JavaScript (ES modules)

## 📊 统计数据

- **总代码量**: 4,000+ 行
- **MCP Server**: 1,088 行 JavaScript
- **文档**: 3,000+ 行 Markdown
- **MCP 工具**: 7 个
- **支持的检查类型**: 4 种（内存、并发、错误、资源）
- **测试类型**: 4 种（正常、边界、并发、压力）

## 💡 核心特性

### 1. 无需本地执行
- 所有分析在 AI 层面完成
- 不需要编译或运行代码
- 用户可以选择性地手动验证

### 2. 知识积累
- 持久化存储分析结果
- 追踪测试覆盖率
- 记录 Bug 和修复历史

### 3. 智能推荐
- 基于优先级排序
- 识别高风险系统调用
- 自动生成工作队列

### 4. 可扩展性
- 易于添加新工具
- 支持自定义检查规则
- 灵活的数据库结构

## 🎯 使用场景

### 场景 1：生成测试用例

```javascript
// 在 Claude Code 中使用 MCP 工具
generate_test_case({
  syscall: "read",
  test_type: "all"
})

// 输出：test-cases/syscall/test_read.c
```

### 场景 2：分析系统调用

```javascript
analyze_syscall({
  syscall: "futex",
  checks: ["memory", "concurrency", "error", "resource"]
})

// 输出：Bug 报告和修复建议
```

### 场景 3：获取推荐

```javascript
suggest_next_target({
  focus: "priority"
})

// 输出：按优先级排序的系统调用列表
```

## 🔍 已知问题

1. **NPM 安全警告**
   - 1 个 high severity 漏洞
   - 运行 `npm audit fix` 修复

2. **测试覆盖**
   - 当前没有单元测试
   - 建议添加 Jest 测试框架

## 📝 开发指南

### 添加新的检查规则

编辑 `mcp-servers/starry-testing/index.js`，在相应的检查函数中添加新规则：

```javascript
checkMemorySafety(content, syscall) {
  // 添加新的检查逻辑
}
```

### 自定义测试模板

修改 `generateTestTemplate` 函数来自定义测试用例的生成逻辑。

### 扩展数据库结构

编辑 `docs/testing/syscall-database.json`，添加新的字段或数据结构。

## 🎓 学习价值

这个项目展示了：
1. **AI 工程能力** - 设计 AI 驱动的自动化系统
2. **系统设计能力** - 分层架构、模块化设计
3. **工程实践** - 完整的文档、测试、配置
4. **问题解决能力** - 识别问题、设计方案、实现验证
5. **技术广度** - Rust、JavaScript、MCP、AI、操作系统

## 📞 支持

如有问题，请参考：
- 主文档：`docs/AI_AUTOMATED_TESTING_SYSTEM.md`
- 快速指南：`docs/AI_SYSTEM_SUMMARY.md`
- MCP 文档：`mcp-servers/starry-testing/README.md`
- 状态报告：`HARNESS_STATUS.md`

## 📄 许可证

本项目遵循 StarryOS 的 Apache-2.0 许可证。

---

**创建日期**: 2026-04-12  
**版本**: 1.0  
**状态**: 基础设施完成，待集成测试
