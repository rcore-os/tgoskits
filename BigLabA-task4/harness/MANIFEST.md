# StarryOS AI Harness - 文件清单

## 📦 完整文件列表

### 根目录文件
- `README.md` - 主说明文档
- `HARNESS_STATUS.md` - 实现状态报告
- `test-harness.md` - 测试验证报告
- `MANIFEST.md` - 本文件（文件清单）
- `.gitignore` - Git 忽略规则

### 📚 docs/ - 文档目录

#### 核心文档
- `AI_AUTOMATED_TESTING_SYSTEM.md` (2,295 行)
  - 完整的系统设计文档
  - 包含 8 个主要章节
  - 从动机到实现的完整阐述

- `AI_SYSTEM_SUMMARY.md` (6.5 KB)
  - 快速参考指南
  - 使用示例
  - 常见问题解答

- `INTERVIEW_AI_HARNESS.md` (642 行)
  - 面试级别的设计文档
  - 详细的设计思路
  - 技术决策记录

#### 数据文件
- `testing/syscall-database.json`
  - 系统调用元数据数据库
  - Bug 追踪数据
  - 测试结果记录

### 🔧 mcp-servers/ - MCP Server 实现

#### starry-testing/
- `index.js` (1,088 行)
  - 主服务器实现
  - 7 个 MCP 工具
  - 数据库管理
  - 测试生成引擎
  - Bug 分析引擎

- `package.json`
  - NPM 包配置
  - 依赖声明
  - 脚本定义

- `package-lock.json`
  - 依赖锁定文件
  - 确保可重现构建

- `README.md`
  - MCP Server 使用文档
  - API 参考
  - 配置说明

- `claude-config-example.json`
  - Claude Code 配置示例
  - 环境变量设置

- `test.js`
  - 简单测试脚本
  - 用于验证基本功能

- `node_modules/` (14 packages)
  - @modelcontextprotocol/sdk
  - zod
  - 其他依赖

### 🧪 test-cases/ - 测试用例目录

- `syscall/` (空目录)
  - 用于存放生成的测试用例
  - 格式：test_<syscall_name>.c

## 📊 统计信息

### 代码量统计
```
文件类型          行数      文件数
----------------------------------------
Markdown        3,937+        7
JavaScript      1,088         1
JSON              100+        3
----------------------------------------
总计            5,125+       11 (不含 node_modules)
```

### 目录大小
```
总大小: 7.7 MB
- node_modules: ~7.5 MB
- 文档和代码: ~200 KB
```

### 功能覆盖
```
MCP 工具:        7/7    (100%)
文档章节:        6/8    (75%)
测试验证:        4/4    (100%)
基础设施:        5/5    (100%)
```

## 🎯 核心文件说明

### 必读文件（按优先级）

1. **README.md**
   - 第一个要看的文件
   - 了解整体结构
   - 快速开始指南

2. **docs/AI_SYSTEM_SUMMARY.md**
   - 快速了解系统功能
   - 包含使用示例
   - 适合快速上手

3. **HARNESS_STATUS.md**
   - 了解当前状态
   - 查看进度和统计
   - 下一步计划

4. **docs/AI_AUTOMATED_TESTING_SYSTEM.md**
   - 深入了解系统设计
   - 完整的技术文档
   - 适合深度学习

5. **docs/INTERVIEW_AI_HARNESS.md**
   - 了解设计动机
   - 技术决策过程
   - 适合面试展示

### 开发文件

1. **mcp-servers/starry-testing/index.js**
   - 核心实现代码
   - 1,088 行 JavaScript
   - 包含所有工具逻辑

2. **mcp-servers/starry-testing/README.md**
   - MCP Server 文档
   - API 参考
   - 工具使用说明

3. **docs/testing/syscall-database.json**
   - 数据库结构
   - 示例数据
   - 可以直接编辑

## 🔄 文件依赖关系

```
README.md
  ├─> docs/AI_SYSTEM_SUMMARY.md (快速参考)
  ├─> HARNESS_STATUS.md (状态报告)
  └─> mcp-servers/starry-testing/README.md (MCP 文档)

mcp-servers/starry-testing/index.js
  ├─> docs/testing/syscall-database.json (读写)
  ├─> test-cases/syscall/*.c (生成)
  └─> package.json (依赖)

docs/AI_AUTOMATED_TESTING_SYSTEM.md
  ├─> 引用所有其他文档
  └─> 完整的系统描述
```

## 📝 使用流程

### 新用户
1. 阅读 `README.md`
2. 阅读 `docs/AI_SYSTEM_SUMMARY.md`
3. 配置 MCP Server
4. 开始使用工具

### 开发者
1. 阅读 `docs/AI_AUTOMATED_TESTING_SYSTEM.md`
2. 查看 `mcp-servers/starry-testing/index.js`
3. 理解数据库结构
4. 开始扩展功能

### 面试准备
1. 阅读 `docs/INTERVIEW_AI_HARNESS.md`
2. 查看 `HARNESS_STATUS.md`
3. 准备演示示例
4. 理解技术决策

## 🚀 快速定位

### 我想...

**了解这个项目是什么**
→ `README.md`

**快速开始使用**
→ `docs/AI_SYSTEM_SUMMARY.md`

**深入了解设计**
→ `docs/AI_AUTOMATED_TESTING_SYSTEM.md`

**准备面试**
→ `docs/INTERVIEW_AI_HARNESS.md`

**查看实现代码**
→ `mcp-servers/starry-testing/index.js`

**配置 MCP Server**
→ `mcp-servers/starry-testing/claude-config-example.json`

**查看测试结果**
→ `test-harness.md`

**了解当前进度**
→ `HARNESS_STATUS.md`

## 📦 打包说明

### 完整打包（包含 node_modules）
```bash
tar -czf starryos-harness-full.tar.gz harness/
# 大小: ~7.7 MB
```

### 精简打包（不含 node_modules）
```bash
tar -czf starryos-harness-slim.tar.gz \
  --exclude='node_modules' \
  --exclude='package-lock.json' \
  harness/
# 大小: ~200 KB
```

### 恢复依赖
```bash
cd harness/mcp-servers/starry-testing
npm install
```

## ✅ 完整性检查

运行以下命令验证所有文件都存在：

```bash
cd harness

# 检查核心文件
test -f README.md && echo "✅ README.md"
test -f HARNESS_STATUS.md && echo "✅ HARNESS_STATUS.md"
test -f test-harness.md && echo "✅ test-harness.md"

# 检查文档
test -f docs/AI_AUTOMATED_TESTING_SYSTEM.md && echo "✅ 系统文档"
test -f docs/AI_SYSTEM_SUMMARY.md && echo "✅ 快速指南"
test -f docs/INTERVIEW_AI_HARNESS.md && echo "✅ 面试文档"
test -f docs/testing/syscall-database.json && echo "✅ 数据库"

# 检查 MCP Server
test -f mcp-servers/starry-testing/index.js && echo "✅ MCP Server"
test -f mcp-servers/starry-testing/package.json && echo "✅ Package.json"
test -f mcp-servers/starry-testing/README.md && echo "✅ MCP 文档"

# 检查目录
test -d test-cases/syscall && echo "✅ 测试用例目录"
test -d mcp-servers/starry-testing/node_modules && echo "✅ 依赖已安装"
```

---

**生成日期**: 2026-04-12  
**版本**: 1.0  
**总文件数**: 11 个核心文件 + node_modules  
**总大小**: 7.7 MB
